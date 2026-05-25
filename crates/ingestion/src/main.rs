//! Ingestion binary.
//!
//! Spawns each venue connector as its own `tokio` task tracked by a
//! [`tokio::task::JoinSet`] so per-venue isolation holds: if Deribit dies or
//! panics, OKX / Bybit (when added in M2+) keep running. Each connector runs
//! under `run_with_retry` (issue #10) and reconnects with exponential backoff
//! on its own; this `main` only wires the topology and the throughput sink.

mod venues;

use std::time::{Duration, Instant};

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
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,volx_ingestion=info")),
        )
        .with_target(false)
        .init();

    info!(
        version = volx_shared_types::METHODOLOGY_VERSION,
        "volx-ingestion starting"
    );

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
    // this, the printer would never see `channel closed` because `main`
    // would keep one Sender alive forever.
    drop(tx);

    let printer = tokio::spawn(log_throughput(rx));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
        finished = drain_venues(&mut venues) => match finished {
            Ok(()) => info!("all venue connectors finished"),
            Err(e) => error!(error = ?e, "venue connector panicked"),
        },
        _ = printer => info!("printer task finished"),
    }
    Ok(())
}

/// Wait for every venue task and log per-venue outcomes. Per-venue isolation:
/// a panic in one task is logged here and the others continue running.
async fn drain_venues(venues: &mut JoinSet<&'static str>) -> Result<()> {
    while let Some(joined) = venues.join_next().await {
        match joined {
            Ok(name) => info!(venue = name, "venue connector exited cleanly"),
            Err(e) if e.is_panic() => warn!(error = ?e, "venue connector panicked"),
            Err(e) => warn!(error = ?e, "venue connector join error"),
        }
    }
    Ok(())
}

async fn log_throughput(rx: flume::Receiver<OptionTick>) {
    let report_every = Duration::from_secs(5);
    let mut total: u64 = 0;
    let mut window_count: u64 = 0;
    let mut last_report = Instant::now();

    while let Ok(tick) = rx.recv_async().await {
        total += 1;
        window_count += 1;
        if last_report.elapsed() >= report_every {
            let window_secs = last_report.elapsed().as_secs_f64();
            #[allow(clippy::cast_precision_loss)]
            let window_rate = window_count as f64 / window_secs;
            info!(
                total = total,
                window_rate_per_s = format!("{window_rate:.1}"),
                window_count = window_count,
                last_asset = ?tick.asset,
                last_strike = tick.strike,
                last_kind = ?tick.kind,
                last_iv = ?tick.iv,
                last_mid = ?tick.mid,
                "throughput"
            );
            last_report = Instant::now();
            window_count = 0;
        }
    }
    info!(total = total, "channel closed");
}
