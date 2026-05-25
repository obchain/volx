//! Ingestion binary.
//!
//! Spawns each venue connector as its own `tokio` task tracked by a
//! [`tokio::task::JoinSet`] so per-venue isolation holds: if Deribit dies or
//! panics, OKX / Bybit (when added in M2+) keep running. Each connector runs
//! under `run_with_retry` (issue #10) and reconnects with exponential backoff
//! on its own.
//!
//! Downstream of the connectors:
//! - issues #12 + #13 filter and dedup the tick stream (`volx-normalizer`)
//! - issue #16 forks each survivor to two sinks:
//!   `ClickHouse` (durable, batched) and Redis pubsub (live fanout).
//!
//! Environment variables (see `.env.example`):
//! - `CLICKHOUSE_URL`  (default `http://127.0.0.1:8123`)
//! - `CLICKHOUSE_DB`   (default `volx`)
//! - `REDIS_URL`       (default `redis://127.0.0.1:6379`)

mod venues;

use anyhow::Result;
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use volx_shared_types::{Asset, OptionTick};

/// Bounded channel between the connector task and downstream sinks.
/// 50 000 is ~50× the acceptance-criterion peak (1 000 ticks/s), so a
/// momentary downstream stall does not back-pressure the WS read loop.
const TICK_CHANNEL_CAPACITY: usize = 50_000;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                EnvFilter::new("info,volx_ingestion=info,volx_normalizer=info")
            }),
        )
        .with_target(false)
        .init();

    info!(
        version = volx_shared_types::METHODOLOGY_VERSION,
        "volx-ingestion starting"
    );

    // Persistence endpoints. Defaults match `.env.example` so a local
    // `docker compose up -d` works out of the box without env wiring.
    let clickhouse_url =
        std::env::var("CLICKHOUSE_URL").unwrap_or_else(|_| "http://127.0.0.1:8123".into());
    let clickhouse_db = std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "volx".into());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    let (tx, rx) = flume::bounded::<OptionTick>(TICK_CHANNEL_CAPACITY);

    // One task per venue. Each owns its own reconnect / backoff loop so a
    // panic or persistent failure on one venue cannot stall the others.
    let mut venues: JoinSet<&'static str> = JoinSet::new();
    {
        let tx = tx.clone();
        venues.spawn(async move {
            venues::deribit::run_with_retry(vec![Asset::Btc, Asset::Eth], tx).await;
            "deribit"
        });
    }
    // Drop the original `tx`: each venue task holds its own clone. Without
    // this, the pipeline would never see `channel closed` because `main`
    // would keep one Sender alive forever.
    drop(tx);

    // Pipeline owns the receiver: filter → dedup → fork(ClickHouse, Redis).
    // Spawned so `main` can race it against ctrl-c + the venue drain.
    let pipeline = tokio::spawn(async move {
        if let Err(e) =
            volx_normalizer::run_default_pipeline(rx, &clickhouse_url, &clickhouse_db, &redis_url)
                .await
        {
            error!(error = %e, "pipeline failed to start");
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
        () = drain_venues(&mut venues) => info!("all venue connectors finished"),
        _ = pipeline => info!("pipeline task finished"),
    }
    Ok(())
}

/// Wait for every venue task and log per-venue outcomes. Per-venue isolation:
/// a panic in one task is logged at `error!` here and the others keep running
/// until they finish on their own terms.
async fn drain_venues(venues: &mut JoinSet<&'static str>) {
    while let Some(joined) = venues.join_next().await {
        match joined {
            Ok(name) => info!(venue = name, "venue connector exited cleanly"),
            Err(e) if e.is_panic() => {
                error!(error = ?e, "venue connector panicked");
            }
            Err(e) => warn!(error = ?e, "venue connector join error (cancelled)"),
        }
    }
}
