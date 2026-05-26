//! BVOL / EVOL index engine — 60-second scheduler (issue #20).
//!
//! Each tick the engine:
//! 1. Pulls the latest tick per `(asset, expiry, strike, kind)` from
//!    `ClickHouse` (`volx.options_ticks`, written by the normalizer).
//! 2. Runs the per-index pipeline (strip builder → variance integral
//!    → 30-day interpolation → BVOL/EVOL conversion).
//! 3. Publishes each surviving [`volx_shared_types::index::IndexValue`]
//!    to `volx.index_ticks` (durable) + Redis `index:{id}:latest`
//!    cache + Redis `index:{id}:stream` pubsub.
//!
//! On any per-index rejection the snapshot is skipped (no row
//! published) and `volx_engine_snapshot_rejected_total{index_id,reason}`
//! increments. The methodology §5 contract of "publish a null row with
//! status" requires a schema bump on `index_ticks` (it has no status
//! column today); that lands in a future PR.
//!
//! ## Dedup
//!
//! `index_ticks` is plain `MergeTree`, not `ReplacingMergeTree`, so a
//! re-INSERT of the same `(index_id, ts)` row accumulates and the
//! `index_1m` materialized view would double-count. Within a single
//! engine process, `tokio::time::interval(60s)` guarantees the
//! per-tick `now` values are at least 60 seconds apart, so the
//! 60-second `index_1m` bucket gets exactly one observation per
//! index. Across engine restarts within the same 60-second bar a
//! duplicate is still possible — a persistent dedup guard (Redis
//! `SET … NX EX 60` keyed on `(index_id, bar)`) belongs to a
//! follow-up hardening PR; not in scope for #20.

use std::time::Duration;

use anyhow::Result;
use clickhouse::Client as ChClient;
use time::OffsetDateTime;
use tokio::signal;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use volx_shared_types::ids::IndexId;

use volx_engine::{chain, sinks::IndexSinks, snapshot};

/// Recompute cadence per `METHODOLOGY.md` §5.
const TICK_INTERVAL: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,volx_engine=info")),
        )
        .with_target(false)
        .init();

    let clickhouse_url =
        std::env::var("CLICKHOUSE_URL").unwrap_or_else(|_| "http://127.0.0.1:8123".into());
    let clickhouse_db = std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "volx".into());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    info!(
        version = volx_shared_types::METHODOLOGY_VERSION,
        clickhouse = %clickhouse_url,
        redis = %redis_url,
        "volx-engine starting"
    );

    // Reader client (the chain query side). Kept separate from the
    // sinks' writer client so the read pool doesn't share max-conn
    // budget with the insert path.
    let reader = ChClient::default()
        .with_url(&clickhouse_url)
        .with_database(&clickhouse_db)
        .with_user("default");
    let mut sinks = IndexSinks::connect(&clickhouse_url, &clickhouse_db, &redis_url).await?;

    let mut ticker = interval(TICK_INTERVAL);
    // `Delay` so a slow tick doesn't immediately fire the next one —
    // we'd rather skip than stack snapshots on top of each other.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Fire the first tick immediately so an operator running the
    // binary sees output without waiting a minute.
    ticker.tick().await;

    info!(interval_s = TICK_INTERVAL.as_secs(), "scheduler running");

    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                return Ok(());
            }
            _ = ticker.tick() => {
                let now = OffsetDateTime::now_utc();
                run_tick(&reader, &mut sinks, now).await;
            }
        }
    }
}

/// Pull chains, run every index, publish or count-the-rejection.
async fn run_tick(reader: &ChClient, sinks: &mut IndexSinks, now: OffsetDateTime) {
    let chains = match chain::fetch_chains(reader, now).await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "chain fetch failed; skipping tick");
            metrics::counter!("volx_engine_tick_errors_total", "stage" => "fetch_chains")
                .increment(1);
            return;
        }
    };

    for index in [IndexId::Bvol, IndexId::Evol] {
        let ticker = index.ticker();
        match snapshot::run_snapshot(&chains, index, now) {
            Ok(res) => {
                if let Err(e) = sinks.publish(&res.value, &res.near, &res.next).await {
                    error!(index_id = ticker, error = %e, "publish failed");
                    metrics::counter!(
                        "volx_engine_publish_errors_total",
                        "index_id" => ticker.to_owned(),
                    )
                    .increment(1);
                } else {
                    info!(
                        index_id = ticker,
                        value = res.value.value,
                        confidence = res.value.confidence,
                        "snapshot published"
                    );
                }
            }
            Err(e) => {
                warn!(index_id = ticker, reason = e.as_label(), error = %e, "snapshot rejected");
                metrics::counter!(
                    "volx_engine_snapshot_rejected_total",
                    "index_id" => ticker.to_owned(),
                    "reason" => e.as_label(),
                )
                .increment(1);
            }
        }
    }
}
