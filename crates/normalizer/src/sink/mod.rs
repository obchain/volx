//! Persistence sinks for normalized option ticks (issue #16).
//!
//! Pipeline shape:
//!
//! ```text
//!     flume::Receiver<OptionTick>
//!                 │
//!                 ▼
//!         filter (Normalizer)
//!                 │
//!                 ▼
//!         dedup  (Deduper)
//!                 │
//!         ┌───────┴───────┐
//!         ▼               ▼
//!   ClickHouseBatcher   RedisPublisher
//!   (durable, batched)  (live fanout, lossy under burst)
//! ```
//!
//! The two sinks have deliberately different reliability postures:
//!
//! - **`ClickHouse`** is the system of record. Inserts batch on `1 s` *or*
//!   `1 000` rows (whichever first, per the acceptance criterion). A flush
//!   failure is logged + counter-emitted but does **not** stall the
//!   pipeline — the next batch retries on its own. The engine reads from
//!   here, so a sustained outage will show up as stale `index_ticks`.
//!
//! - **Redis pubsub** is best-effort fanout for live consumers (the Go API
//!   layer `/v1/stream`, dashboards, etc.). A publish failure or a full
//!   send-queue drops the *oldest* pending message and increments
//!   `volx_normalizer_redis_dropped_total{reason}`. Subscribers that need
//!   guaranteed delivery should read `ClickHouse`, not Redis.
//!
//! ## Backpressure
//!
//! The pipeline is **not** allowed to back-pressure the ingestion WS reader
//! — every survivor is handed to the sinks via *bounded* channels, and the
//! sinks drop on overflow. This keeps the WS read loop's pacing decoupled
//! from any storage hiccup.

pub mod clickhouse;
pub mod redis;

use std::time::Duration;

use ::time::OffsetDateTime;
use flume::Receiver;
use tracing::{debug, info};
use volx_shared_types::OptionTick;

use crate::dedup::{DedupOutcome, Deduper};
use crate::{FilterOutcome, Normalizer};

pub use clickhouse::{ClickHouseBatcher, ClickHouseSinkConfig};
pub use redis::{RedisPublisher, RedisSinkConfig};

/// Drive the normalize → dedup → fork(`ClickHouse`, Redis) pipeline.
///
/// Returns when the source channel closes. Sink errors are logged but never
/// surface as a pipeline-level failure — the goal is to keep the WS reader
/// running even if storage briefly hiccups.
///
/// `clock` is injected for tests; production passes `OffsetDateTime::now_utc`.
pub async fn run_pipeline<F>(
    rx: Receiver<OptionTick>,
    normalizer: Normalizer,
    mut deduper: Deduper,
    clickhouse: ClickHouseBatcher,
    redis: RedisPublisher,
    mut clock: F,
) where
    F: FnMut() -> OffsetDateTime + Send,
{
    info!(
        ch_max_rows = clickhouse.config().max_rows,
        ch_max_age_s = clickhouse.config().max_age.as_secs_f64(),
        redis_cap = redis.config().queue_capacity,
        "normalizer pipeline started"
    );

    while let Ok(tick) = rx.recv_async().await {
        let now = clock();

        if let FilterOutcome::Drop(_) = normalizer.check_tick(&tick, now) {
            continue;
        }
        if deduper.check(&tick, now) == DedupOutcome::Duplicate {
            continue;
        }

        // Both `send` are non-blocking — they push into the sink's own
        // bounded queue with drop-oldest on overflow.
        clickhouse.send(tick.clone());
        redis.send(tick);
    }

    debug!("source channel closed, flushing sinks");
    clickhouse.shutdown().await;
    redis.shutdown().await;
    info!("normalizer pipeline drained cleanly");
}

/// Helper that wires the pipeline against `.env.example` defaults. The
/// ingestion binary calls this; tests build the components by hand for
/// full control.
pub async fn run_default_pipeline(
    rx: Receiver<OptionTick>,
    clickhouse_url: &str,
    clickhouse_db: &str,
    redis_url: &str,
) -> anyhow::Result<()> {
    let normalizer = Normalizer::with_defaults();
    let deduper = Deduper::with_defaults();

    let ch_config = ClickHouseSinkConfig {
        url: clickhouse_url.to_owned(),
        database: clickhouse_db.to_owned(),
        max_rows: 1_000,
        max_age: Duration::from_secs(1),
        ..ClickHouseSinkConfig::default()
    };
    let ch = ClickHouseBatcher::spawn(ch_config)?;

    let redis_config = RedisSinkConfig {
        url: redis_url.to_owned(),
        ..RedisSinkConfig::default()
    };
    let redis = RedisPublisher::spawn(redis_config).await?;

    run_pipeline(rx, normalizer, deduper, ch, redis, OffsetDateTime::now_utc).await;

    Ok(())
}

/// Wire-side label for `Venue`. Matches both the `LowCardinality(String)`
/// value written to `options_ticks.venue` and the Redis topic segment.
pub(crate) fn venue_label(v: volx_shared_types::Venue) -> &'static str {
    match v {
        volx_shared_types::Venue::Deribit => "deribit",
        volx_shared_types::Venue::Okx => "okx",
        volx_shared_types::Venue::Bybit => "bybit",
    }
}

pub(crate) fn asset_label(a: volx_shared_types::Asset) -> &'static str {
    match a {
        volx_shared_types::Asset::Btc => "btc",
        volx_shared_types::Asset::Eth => "eth",
    }
}

pub(crate) fn kind_label(k: volx_shared_types::OptionKind) -> &'static str {
    match k {
        volx_shared_types::OptionKind::Call => "call",
        volx_shared_types::OptionKind::Put => "put",
    }
}
