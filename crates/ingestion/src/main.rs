//! Ingestion binary.
//!
//! Spawns the per-venue connectors (only Deribit lands in #9) and drains the
//! resulting `OptionTick` stream, logging throughput every 5 s. Downstream
//! consumers (normalizer, `ClickHouse` writer, in-process subscribers) will
//! replace the throughput-logger receiver in later milestones.

mod venues;

use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{error, info};
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

    let connector = tokio::spawn(async move {
        if let Err(e) = venues::deribit::connect_and_stream(&[Asset::Btc, Asset::Eth], tx).await {
            error!(error = ?e, "deribit connector exited with error");
        }
    });

    let printer = tokio::spawn(log_throughput(rx));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
        _ = connector => info!("connector task finished"),
        _ = printer => info!("printer task finished"),
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
