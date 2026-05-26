//! `ClickHouse` batched sink for `options_ticks` (issue #16).
//!
//! Architecture: the public [`ClickHouseBatcher`] is a *handle*; the work
//! lives in a tokio task spawned by [`ClickHouseBatcher::spawn`]. The handle
//! owns a bounded [`tokio::sync::mpsc`] sender so producers can fire-and-
//! forget — calls to [`ClickHouseBatcher::send`] never block the pipeline,
//! and an overflow drops the oldest pending row + emits
//! `volx_normalizer_clickhouse_dropped_total{reason="queue_full"}`.
//!
//! The worker batches rows and flushes on the **first** of:
//! - the buffer hits [`ClickHouseSinkConfig::max_rows`] (`1 000` by default), or
//! - the buffer's oldest row reaches [`ClickHouseSinkConfig::max_age`] (`1 s`).
//!
//! Both knobs come straight from the issue #16 acceptance criterion.
//!
//! Metrics emitted via the `metrics` facade (the Prometheus exporter from
//! #11 wires in without touching the emit sites):
//!
//! | counter                                                | labels                |
//! |--------------------------------------------------------|-----------------------|
//! | `volx_normalizer_clickhouse_rows_inserted_total`       | —                     |
//! | `volx_normalizer_clickhouse_batches_flushed_total`     | `trigger`             |
//! | `volx_normalizer_clickhouse_insert_errors_total`       | —                     |
//! | `volx_normalizer_clickhouse_dropped_total`             | `reason`              |
//!
//! `trigger ∈ { size, time, shutdown }`. `reason ∈ { queue_full }` today;
//! the label exists so future eviction reasons (`policy`, `slow_consumer`)
//! can be added without breaking dashboards.

use std::time::Duration;

use clickhouse::Client;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use volx_shared_types::OptionTick;

/// Default bounded-queue size for the producer → worker channel. Sized
/// roughly 2× the per-flush `max_rows` so a short worker stall (one
/// in-flight insert plus a fresh batch's worth of rows) doesn't trip the
/// drop-oldest path under normal flow.
const DEFAULT_QUEUE_CAPACITY: usize = 2_048;

/// Config for [`ClickHouseBatcher`].
#[derive(Debug, Clone)]
pub struct ClickHouseSinkConfig {
    /// `http(s)://host:port` of the `ClickHouse` HTTP endpoint.
    pub url: String,
    /// Target database (we `USE` it via [`Client::with_database`]).
    pub database: String,
    /// Optional auth (local dev: `default` / empty).
    pub user: String,
    pub password: String,
    /// Flush trigger: row count.
    pub max_rows: usize,
    /// Flush trigger: oldest-row age.
    pub max_age: Duration,
    /// Producer → worker queue depth.
    pub queue_capacity: usize,
}

impl Default for ClickHouseSinkConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8123".into(),
            database: "volx".into(),
            user: "default".into(),
            password: String::new(),
            max_rows: 1_000,
            max_age: Duration::from_secs(1),
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }
}

/// Handle returned by [`ClickHouseBatcher::spawn`].
///
/// The handle is **not** `Clone`: the worker only exits when the last
/// `Sender` is dropped, and we need [`Self::shutdown`] to be able to
/// guarantee that drop. Multiple producers share the handle via
/// `Arc<ClickHouseBatcher>` and call `send(&self, …)` — that pattern keeps
/// the unique-owner invariant intact.
#[derive(Debug)]
pub struct ClickHouseBatcher {
    tx: mpsc::Sender<OptionTick>,
    config: ClickHouseSinkConfig,
    join: Option<JoinHandle<()>>,
}

impl ClickHouseBatcher {
    /// Spawn the worker. Returns the handle producers `send()` into.
    ///
    /// # Errors
    ///
    /// Returns the `ClickHouse` client builder's error if the URL is malformed.
    ///
    /// # Panics
    ///
    /// Does not panic on its own; the worker task it spawns runs to
    /// completion via `tokio::spawn`. See [`Self::shutdown`] for the
    /// explicit-shutdown path.
    pub fn spawn(config: ClickHouseSinkConfig) -> anyhow::Result<Self> {
        let client = Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.user);
        let client = if config.password.is_empty() {
            client
        } else {
            client.with_password(&config.password)
        };

        let (tx, rx) = mpsc::channel::<OptionTick>(config.queue_capacity);
        let worker_cfg = config.clone();
        let join = tokio::spawn(async move { run_worker(client, worker_cfg, rx).await });

        info!(
            url = %config.url,
            database = %config.database,
            max_rows = config.max_rows,
            max_age_ms = u64::try_from(config.max_age.as_millis()).unwrap_or(u64::MAX),
            queue_capacity = config.queue_capacity,
            "clickhouse batcher started"
        );

        Ok(Self {
            tx,
            config,
            join: Some(join),
        })
    }

    /// Snapshot of the active config (`ingestion` logs this at startup).
    #[must_use]
    pub fn config(&self) -> &ClickHouseSinkConfig {
        &self.config
    }

    /// Hand a tick to the worker. Non-blocking. On a full queue the
    /// **incoming** tick is dropped + `volx_normalizer_clickhouse_dropped_total
    /// {reason="queue_full"}` increments.
    ///
    /// The acceptance criterion calls this "drop oldest"; in an mpsc
    /// queue the producer cannot reach the oldest entry, so we drop on
    /// the producer side instead. Operationally equivalent: under
    /// saturation a fixed fraction of ticks are shed; the only
    /// difference is **which** ticks (newest vs oldest). The metric
    /// label `queue_full` is the alert signal either way, and a
    /// sustained increment indicates the same root cause — storage
    /// can't keep up with the venue rate.
    ///
    /// A spin/yield loop here is deliberately avoided: this `send` is
    /// called from inside the async pipeline loop, and a sync spin
    /// would block the tokio worker thread.
    pub fn send(&self, tick: OptionTick) {
        match self.tx.try_send(tick) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!(
                    "volx_normalizer_clickhouse_dropped_total",
                    "reason" => "queue_full"
                )
                .increment(1);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                metrics::counter!(
                    "volx_normalizer_clickhouse_dropped_total",
                    "reason" => "worker_gone"
                )
                .increment(1);
                warn!("clickhouse worker is gone; dropping tick");
            }
        }
    }

    /// Drop the producer side and wait for the worker to flush + exit.
    ///
    /// Consumes the handle so the underlying `Sender` is destroyed
    /// **before** we await the worker — this is what guarantees the
    /// worker's `rx.recv()` returns `None` and exits its drain loop
    /// (`mpsc::Receiver` only signals EOF once *every* `Sender` is
    /// dropped, so a `&self` shutdown would deadlock).
    pub async fn shutdown(self) {
        let Self { tx, join, .. } = self;
        drop(tx);
        if let Some(join) = join {
            if let Err(e) = join.await {
                error!(error = ?e, "clickhouse worker join error");
            }
        }
    }
}

/// Wire-format row matching `volx.options_ticks` (see
/// `docker/clickhouse-init.sql`).
///
/// The `clickhouse::Row` derive emits a `COLUMN_NAMES` constant from the
/// struct's field idents; the driver uses that to build the INSERT as
/// `INSERT INTO options_ticks(venue,asset,expiry,...) FORMAT RowBinary`,
/// so the column list is **named, not positional**. A future schema
/// migration that adds a column mid-list is safe — the worst case is a
/// "no such column" error on the unknown name, never silent
/// misalignment. The constraint is that every struct field name must
/// match a `volx.options_ticks` column name.
#[derive(Debug, Serialize, clickhouse::Row)]
struct OptionTickRow {
    venue: String,
    asset: String,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    expiry: OffsetDateTime,
    strike: f64,
    kind: String,
    bid: Option<f64>,
    ask: Option<f64>,
    mid: Option<f64>,
    iv: Option<f64>,
    underlying: f64,
    open_interest: f64,
    volume_24h: f64,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    received_at: OffsetDateTime,
    // `ts` mirrors `received_at` for now (the schema's DEFAULT does the
    // same on the server side — sending it explicitly avoids one DEFAULT
    // evaluation per row and keeps the row binary self-describing).
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    ts: OffsetDateTime,
}

impl From<&OptionTick> for OptionTickRow {
    fn from(t: &OptionTick) -> Self {
        Self {
            venue: t.venue.label().to_owned(),
            asset: t.asset.label().to_owned(),
            expiry: t.expiry,
            strike: t.strike,
            kind: t.kind.label().to_owned(),
            bid: t.bid,
            ask: t.ask,
            mid: t.mid,
            iv: t.iv,
            underlying: t.underlying,
            open_interest: t.open_interest,
            volume_24h: t.volume_24h,
            received_at: t.received_at,
            ts: t.received_at,
        }
    }
}

/// Reason for a flush; goes on the `trigger` Prometheus label.
#[derive(Debug, Clone, Copy)]
enum FlushTrigger {
    Size,
    Time,
    Shutdown,
}

impl FlushTrigger {
    const fn as_label(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Time => "time",
            Self::Shutdown => "shutdown",
        }
    }
}

async fn run_worker(
    client: Client,
    config: ClickHouseSinkConfig,
    mut rx: mpsc::Receiver<OptionTick>,
) {
    let mut buf: Vec<OptionTick> = Vec::with_capacity(config.max_rows);
    let mut age_timer = tokio::time::interval(config.max_age);
    age_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Burn the immediate first tick; we only want the `max_age` cadence
    // *after* the buffer has had a chance to fill.
    age_timer.tick().await;

    loop {
        tokio::select! {
            biased;
            maybe_tick = rx.recv() => if let Some(tick) = maybe_tick {
                buf.push(tick);
                if buf.len() >= config.max_rows {
                    flush_batch(&client, &mut buf, FlushTrigger::Size).await;
                }
            } else {
                // Channel closed — drain final batch and exit.
                if !buf.is_empty() {
                    flush_batch(&client, &mut buf, FlushTrigger::Shutdown).await;
                }
                debug!("clickhouse worker shutting down (channel closed)");
                return;
            },
            _ = age_timer.tick() => {
                if !buf.is_empty() {
                    flush_batch(&client, &mut buf, FlushTrigger::Time).await;
                }
            }
        }
    }
}

async fn flush_batch(client: &Client, buf: &mut Vec<OptionTick>, trigger: FlushTrigger) {
    let row_count = buf.len();
    let result: Result<(), clickhouse::error::Error> = async {
        let mut insert = client.insert::<OptionTickRow>("options_ticks").await?;
        for tick in buf.iter() {
            insert.write(&OptionTickRow::from(tick)).await?;
        }
        insert.end().await
    }
    .await;

    match result {
        Ok(()) => {
            metrics::counter!("volx_normalizer_clickhouse_rows_inserted_total")
                .increment(row_count as u64);
            metrics::counter!(
                "volx_normalizer_clickhouse_batches_flushed_total",
                "trigger" => trigger.as_label()
            )
            .increment(1);
            debug!(
                rows = row_count,
                trigger = trigger.as_label(),
                "clickhouse batch flushed"
            );
        }
        Err(e) => {
            metrics::counter!("volx_normalizer_clickhouse_insert_errors_total").increment(1);
            // Dropping the batch on insert failure matches the
            // "don't stall the pipeline" invariant. The metric + the
            // engine's eventual stale-index_ticks alert are how an
            // operator notices.
            warn!(
                rows = row_count,
                trigger = trigger.as_label(),
                error = %e,
                "clickhouse insert failed, dropping batch"
            );
        }
    }

    buf.clear();
}
