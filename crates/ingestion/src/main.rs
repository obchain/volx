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
//! Observability (issue #11):
//! - Prometheus exporter on `127.0.0.1:9100/metrics` — localhost-only by
//!   default so a misconfigured firewall can't expose internal counters
//!   LAN-wide. Override via `METRICS_BIND`.
//! - Tracing format defaults to ANSI text; set `RUST_LOG_FORMAT=json`
//!   for structured logs (one event per line, JSON encoded) — useful
//!   when piping into Loki / ELK.
//!
//! Environment variables (see `.env.example`):
//! - `CLICKHOUSE_URL`  (default `http://127.0.0.1:8123`)
//! - `CLICKHOUSE_DB`   (default `volx`)
//! - `REDIS_URL`       (default `redis://127.0.0.1:6379`)
//! - `METRICS_BIND`    (default `127.0.0.1:9100`)
//! - `RUST_LOG_FORMAT` (default `text`, set to `json` for structured)

mod venues;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use metrics_exporter_prometheus::PrometheusBuilder;
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
    init_tracing();

    info!(
        version = volx_shared_types::METHODOLOGY_VERSION,
        "volx-ingestion starting"
    );

    install_prometheus_exporter()?;

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
    {
        let tx = tx.clone();
        venues.spawn(async move {
            venues::okx::run_with_retry(vec![Asset::Btc, Asset::Eth], tx).await;
            "okx"
        });
    }
    {
        let tx = tx.clone();
        venues.spawn(async move {
            venues::bybit::run_with_retry(vec![Asset::Btc, Asset::Eth], tx).await;
            "bybit"
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

/// Tracing subscriber wiring. Default text format for dev ergonomics;
/// `RUST_LOG_FORMAT=json` switches to one-JSON-event-per-line which is
/// what `vector` / `promtail` / `fluent-bit` want as input.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,volx_ingestion=info,volx_normalizer=info"));

    if std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
}

/// Install the Prometheus exporter that serves `/metrics` on
/// `METRICS_BIND` (default `127.0.0.1:9100`). The recorder it
/// installs is the backing store for every `metrics::counter!` /
/// `gauge!` / `histogram!` call in the workspace — emit sites
/// (normalizer, ingestion) do not change.
fn install_prometheus_exporter() -> Result<()> {
    let bind = std::env::var("METRICS_BIND").unwrap_or_else(|_| "127.0.0.1:9100".into());
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid METRICS_BIND `{bind}`"))?;

    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .context("failed to install Prometheus exporter")?;

    info!(bind = %addr, path = "/metrics", "Prometheus exporter listening");
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
