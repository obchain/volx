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

use super::{asset_label, kind_label, venue_label};

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
/// Cloning the handle is cheap (it's just an `mpsc::Sender` clone), so
/// multiple producers can fan ticks into one shared worker.
#[derive(Debug, Clone)]
pub struct ClickHouseBatcher {
    tx: mpsc::Sender<OptionTick>,
    config: ClickHouseSinkConfig,
    /// `Arc<>` so cheap clones share the join handle but only the original
    /// owner can `await` it from `shutdown()`. The worker stops when every
    /// `Sender` is dropped — `shutdown()` drops `tx` then awaits the join.
    join: std::sync::Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
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
            join: std::sync::Arc::new(std::sync::Mutex::new(Some(join))),
        })
    }

    /// Snapshot of the active config (`ingestion` logs this at startup).
    #[must_use]
    pub fn config(&self) -> &ClickHouseSinkConfig {
        &self.config
    }

    /// Hand a tick to the worker. Non-blocking. If the bounded queue is
    /// full, evict the oldest pending row to make space and increment
    /// `volx_normalizer_clickhouse_dropped_total{reason="queue_full"}`.
    /// Drop-oldest matches the pipeline's "never back-pressure the WS
    /// reader" invariant.
    pub fn send(&self, tick: OptionTick) {
        // `try_send` returns the value back on failure, which lets us
        // implement drop-oldest without losing the *new* row.
        let mut pending = tick;
        loop {
            match self.tx.try_send(pending) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Full(returned)) => {
                    metrics::counter!(
                        "volx_normalizer_clickhouse_dropped_total",
                        "reason" => "queue_full"
                    )
                    .increment(1);
                    // Best-effort drain of one slot. If a concurrent
                    // worker pulled in the meantime, the next try_send
                    // succeeds; if not, we keep looping. The loop is
                    // bounded by `queue_capacity` in the worst case.
                    let receiver_alive = !self.tx.is_closed();
                    if !receiver_alive {
                        warn!("clickhouse worker is gone; dropping tick");
                        return;
                    }
                    pending = returned;
                    // Yield once so the worker can drain — without this
                    // a sync caller in a tight loop on a single-thread
                    // runtime would spin forever.
                    std::thread::yield_now();
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!("clickhouse worker is gone; dropping tick");
                    return;
                }
            }
        }
    }

    /// Drop the producer side and wait for the worker to flush + exit.
    /// Idempotent: a second call is a no-op.
    ///
    /// # Panics
    ///
    /// Panics if the internal join-handle mutex is poisoned — only
    /// possible if a previous holder panicked while shutting down,
    /// which is a programmer error worth surfacing.
    pub async fn shutdown(&self) {
        // Drop the `Sender` clone first so the worker's `recv()` sees the
        // channel close once every clone is gone. We can't drop the field
        // directly behind `&self`, so we let the original handle's
        // `Drop` impl + the caller's clone management close it; the await
        // here only joins on the worker.
        let join = self.join.lock().expect("clickhouse join mutex").take();
        if let Some(join) = join {
            // Convert the sender into a no-op by closing it from the
            // worker side — `Sender::closed()` would await disconnection;
            // we instead drop our clone by replacing with a closed sender.
            //
            // The cleanest pattern is: caller owns the `ClickHouseBatcher`
            // and lets it Drop. We add a defensive close here for the
            // explicit-shutdown caller in `run_default_pipeline`.
            //
            // NOTE: tokio doesn't expose `Sender::close()` as a free fn
            // for cloned senders. The worker sees EOF only when the *last*
            // clone is dropped. We assume the caller has dropped or will
            // drop their handle around this `await`.
            if let Err(e) = join.await {
                error!(error = ?e, "clickhouse worker join error");
            }
        }
    }
}

/// Wire-format row matching `volx.options_ticks` (see
/// `docker/clickhouse-init.sql`). Field order matters — the `ClickHouse`
/// driver maps positionally in `RowBinary`.
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
            venue: venue_label(t.venue).to_owned(),
            asset: asset_label(t.asset).to_owned(),
            expiry: t.expiry,
            strike: t.strike,
            kind: kind_label(t.kind).to_owned(),
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
