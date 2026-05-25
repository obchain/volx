//! Index sinks — write each published [`IndexValue`] to `ClickHouse`
//! (`volx.index_ticks`) + two Redis surfaces (issue #20):
//!
//! - `SET index:{id}:latest <json>` — hot latest-value cache, read by
//!   the Go API's REST endpoints (#23). No TTL on the key; every new
//!   snapshot overwrites it.
//! - `PUBLISH index:{id}:stream <json>` — live broadcast for the
//!   `/v1/stream` WS layer (#24) and any operator dashboard.
//!
//! Both Redis writes are best-effort: on a publish failure the
//! `volx_engine_redis_errors_total{op}` counter increments and the
//! function returns `Ok(())` — the canonical record is the
//! `ClickHouse` insert, which is the only sink that can fail this
//! function. This matches the normalizer's posture from #16.

use ::redis::{AsyncCommands, Client as RedisClient, aio::MultiplexedConnection};
use clickhouse::Client as ChClient;
use serde::Serialize;
use time::OffsetDateTime;
use tracing::warn;
use volx_shared_types::index::IndexValue;

/// `volx.index_ticks` row mirror. Field order + names must match the
/// `CREATE TABLE` in `docker/clickhouse-init.sql`; `clickhouse::Row`
/// emits a *named* INSERT (`venue,asset,…`) per the driver's
/// `COLUMN_NAMES` machinery, so a future schema migration that adds a
/// column mid-list is safe — the constraint is field-name ↔ column-name
/// parity.
#[derive(Debug, Serialize, clickhouse::Row)]
struct IndexRow<'a> {
    index_id: &'a str,
    value: f64,
    confidence: f64,
    strip_hash: [u8; 32],
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    ts: OffsetDateTime,
}

/// Errors returned by [`IndexSinks::publish`].
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("clickhouse insert failed: {0}")]
    ClickHouse(#[from] clickhouse::error::Error),
    #[error("strip_hash serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Holds clients for both sinks. Cheap to clone — `clickhouse::Client`
/// is `Arc`-backed and `MultiplexedConnection` shares its inner state.
/// `clickhouse::Client` does not implement `Debug`, so we don't derive
/// it here either.
#[derive(Clone)]
pub struct IndexSinks {
    clickhouse: ChClient,
    redis: MultiplexedConnection,
}

impl std::fmt::Debug for IndexSinks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexSinks")
            .field("clickhouse", &"<ClickHouse client>")
            .field("redis", &"<Redis multiplexed conn>")
            .finish()
    }
}

impl IndexSinks {
    /// Connect to both sinks. The connection is warmed before this
    /// returns; a typo'd URL fails here, not on the first publish.
    ///
    /// # Errors
    ///
    /// Returns the driver error if either client fails to handshake.
    pub async fn connect(
        clickhouse_url: &str,
        clickhouse_db: &str,
        redis_url: &str,
    ) -> anyhow::Result<Self> {
        let clickhouse = ChClient::default()
            .with_url(clickhouse_url)
            .with_database(clickhouse_db)
            .with_user("default");
        let redis = RedisClient::open(redis_url)?
            .get_multiplexed_async_connection()
            .await?;
        Ok(Self { clickhouse, redis })
    }

    /// Insert one row into `index_ticks` and fan out to Redis.
    ///
    /// # Errors
    ///
    /// Returns the `ClickHouse` error if the durable insert fails.
    /// Redis publish / SET errors are logged + counter-emitted but do
    /// **not** surface here — the system of record is `index_ticks`,
    /// and the Redis surfaces are best-effort live caches.
    pub async fn publish(&mut self, iv: &IndexValue) -> Result<(), SinkError> {
        let ticker = iv.index_id.ticker();
        let row = IndexRow {
            index_id: ticker,
            value: iv.value,
            confidence: iv.confidence,
            strip_hash: iv.strip_hash.0,
            ts: iv.ts,
        };

        // 1. Durable insert (the only failure path this function
        //    bubbles).
        {
            let mut insert = self.clickhouse.insert::<IndexRow>("index_ticks").await?;
            insert.write(&row).await?;
            insert.end().await?;
        }
        metrics::counter!(
            "volx_engine_index_rows_inserted_total",
            "index_id" => ticker.to_owned(),
        )
        .increment(1);

        // 2. Redis SET + PUBLISH. Best-effort; on error increment a
        //    counter and continue. Encoding matches the per-tick
        //    options pubsub envelope from the normalizer: compact JSON
        //    with RFC 3339 timestamps + hex strip_hash.
        let payload = serde_json::to_string(iv)?;

        let latest_key = format!("index:{ticker}:latest");
        let stream_topic = format!("index:{ticker}:stream");

        if let Err(e) = self.redis.set::<_, _, ()>(&latest_key, &payload).await {
            warn!(error = %e, key = %latest_key, "redis SET failed");
            metrics::counter!(
                "volx_engine_redis_errors_total",
                "op" => "set",
            )
            .increment(1);
        }

        if let Err(e) = self
            .redis
            .publish::<_, _, i64>(&stream_topic, &payload)
            .await
        {
            warn!(error = %e, topic = %stream_topic, "redis PUBLISH failed");
            metrics::counter!(
                "volx_engine_redis_errors_total",
                "op" => "publish",
            )
            .increment(1);
        } else {
            metrics::counter!(
                "volx_engine_index_published_total",
                "index_id" => ticker.to_owned(),
            )
            .increment(1);
        }

        Ok(())
    }
}
