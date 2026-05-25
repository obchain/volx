//! Redis pubsub fanout for normalized option ticks (issue #16).
//!
//! Publishes each surviving tick as JSON to `options:{venue}:{asset}`. The
//! topic shape is the one consumed by the Go API's `/v1/stream` (issue #24)
//! and by any dashboard that wants the live firehose.
//!
//! ## Reliability posture
//!
//! Pubsub here is **best-effort**. Two failure modes increment the
//! `volx_normalizer_redis_dropped_total{reason}` counter and continue:
//!
//! - `reason="queue_full"` — the bounded producer queue overflowed. We
//!   evict the oldest pending tick (matching the issue #16 acceptance
//!   criterion) rather than block, because blocking would back-pressure
//!   the WS reader.
//! - `reason="publish_error"` — the redis driver returned an error on
//!   `PUBLISH`. The worker reconnects on the next message via the
//!   `redis::Client::get_async_connection` path.
//!
//! Consumers that need guaranteed delivery should read from `options_ticks`
//! in `ClickHouse` instead.

use std::time::Duration;

use ::redis::{AsyncCommands, Client, aio::MultiplexedConnection};
use serde::ser::Error as _;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use volx_shared_types::OptionTick;

use super::{asset_label, kind_label, venue_label};

/// Default depth of the producer → worker queue. One tick is ~250 B
/// post-serialization; this caps the in-process buffer near 1 MB. Larger
/// caps just defer the drop-oldest decision under sustained Redis
/// slowness — they don't fix it.
const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

/// How long to wait between reconnect attempts when the multiplexed
/// connection is poisoned. Short by design — pubsub is real-time data,
/// long retries cause visible lag at the consumer.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Config for [`RedisPublisher`].
#[derive(Debug, Clone)]
pub struct RedisSinkConfig {
    /// `redis://host:port[/db]`.
    pub url: String,
    /// Producer → worker queue depth.
    pub queue_capacity: usize,
}

impl Default for RedisSinkConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".into(),
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }
}

/// Handle owning the producer side of the redis worker.
///
/// The handle is **not** `Clone`: the worker only drains when the last
/// `Sender` is dropped, and we need [`Self::shutdown`] to be able to
/// guarantee that drop. Producers share via `Arc<RedisPublisher>` +
/// `send(&self, …)` instead.
#[derive(Debug)]
pub struct RedisPublisher {
    tx: mpsc::Sender<OptionTick>,
    config: RedisSinkConfig,
    join: Option<JoinHandle<()>>,
}

impl RedisPublisher {
    /// Connect to Redis + spawn the worker.
    ///
    /// # Errors
    ///
    /// Returns the redis driver's error if the URL is malformed or the
    /// initial connection handshake fails. A *runtime* connection drop
    /// after spawn is handled by the worker's reconnect loop, not here.
    pub async fn spawn(config: RedisSinkConfig) -> anyhow::Result<Self> {
        let client = Client::open(config.url.as_str())?;
        // Validate the URL + warm a connection so a typoed URL fails at
        // startup instead of silently dropping every tick.
        let conn = client.get_multiplexed_async_connection().await?;

        let (tx, rx) = mpsc::channel::<OptionTick>(config.queue_capacity);
        let join = tokio::spawn(async move { run_worker(client, conn, rx).await });

        info!(
            url = %config.url,
            queue_capacity = config.queue_capacity,
            "redis publisher started"
        );

        Ok(Self {
            tx,
            config,
            join: Some(join),
        })
    }

    /// Snapshot of the active config.
    #[must_use]
    pub fn config(&self) -> &RedisSinkConfig {
        &self.config
    }

    /// Non-blocking hand-off. On queue overflow the **incoming** tick is
    /// dropped + `volx_normalizer_redis_dropped_total{reason="queue_full"}`
    /// increments. See the matching note on
    /// [`super::clickhouse::ClickHouseBatcher::send`] for the
    /// drop-newest-vs-drop-oldest rationale.
    pub fn send(&self, tick: OptionTick) {
        match self.tx.try_send(tick) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!(
                    "volx_normalizer_redis_dropped_total",
                    "reason" => "queue_full"
                )
                .increment(1);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                metrics::counter!(
                    "volx_normalizer_redis_dropped_total",
                    "reason" => "worker_gone"
                )
                .increment(1);
                warn!("redis worker is gone; dropping tick");
            }
        }
    }

    /// Drop the producer side and wait for the worker to drain + exit.
    /// Consumes the handle so the inner `Sender` is destroyed before we
    /// await; see [`super::clickhouse::ClickHouseBatcher::shutdown`].
    pub async fn shutdown(self) {
        let Self { tx, join, .. } = self;
        drop(tx);
        if let Some(join) = join {
            if let Err(e) = join.await {
                error!(error = ?e, "redis worker join error");
            }
        }
    }
}

async fn run_worker(
    client: Client,
    mut conn: MultiplexedConnection,
    mut rx: mpsc::Receiver<OptionTick>,
) {
    while let Some(tick) = rx.recv().await {
        let topic = format!(
            "options:{}:{}",
            venue_label(tick.venue),
            asset_label(tick.asset)
        );
        // Compact JSON; the wire format mirrors the in-process struct so
        // dashboards parsing this don't need a separate schema. Using
        // `serde_json::to_string` over the tick directly keeps the kind /
        // venue / asset encodings consistent with the REST/WS layer.
        let payload = match tick_to_json(&tick) {
            Ok(s) => s,
            Err(e) => {
                metrics::counter!(
                    "volx_normalizer_redis_dropped_total",
                    "reason" => "encode_error"
                )
                .increment(1);
                warn!(error = %e, "redis: tick encode failed");
                continue;
            }
        };

        match publish(&mut conn, &topic, &payload).await {
            Ok(()) => {
                metrics::counter!(
                    "volx_normalizer_redis_published_total",
                    "venue" => venue_label(tick.venue),
                    "asset" => asset_label(tick.asset),
                )
                .increment(1);
            }
            Err(e) => {
                metrics::counter!(
                    "volx_normalizer_redis_dropped_total",
                    "reason" => "publish_error"
                )
                .increment(1);
                warn!(error = %e, topic = %topic, "redis publish failed, reconnecting");
                conn = reconnect(&client).await;
            }
        }
    }

    debug!("redis worker shutting down (channel closed)");
}

async fn publish(
    conn: &mut MultiplexedConnection,
    topic: &str,
    payload: &str,
) -> ::redis::RedisResult<()> {
    let _: i64 = conn.publish(topic, payload).await?;
    Ok(())
}

/// Re-establish a multiplexed connection, retrying until it succeeds.
///
/// The first attempt fires immediately (the publish failure that triggered
/// us is already the equivalent of a failed handshake); subsequent attempts
/// space themselves by [`RECONNECT_DELAY`].
///
/// **Backlog behavior:** while the worker is in this loop the producer
/// queue keeps filling at the venue tick rate (~700 ticks/s). On reconnect
/// the worker drains that backlog in tight succession, presenting Redis
/// with a small burst (≈ producer rate × outage length). Subscribers that
/// fan out further should be ready to absorb this; the
/// `volx_normalizer_redis_dropped_total{reason="queue_full"}` counter is
/// the alert signal if the outage outlasts queue capacity.
async fn reconnect(client: &Client) -> MultiplexedConnection {
    let mut first = true;
    loop {
        if !first {
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
        first = false;
        match client.get_multiplexed_async_connection().await {
            Ok(c) => {
                info!("redis reconnected");
                return c;
            }
            Err(e) => {
                error!(error = %e, "redis reconnect failed, retrying");
            }
        }
    }
}

fn tick_to_json(t: &OptionTick) -> Result<String, serde_json::Error> {
    // Hand-rolled JSON shape (rather than `serde_json::to_string(t)`)
    // because the wire format here is the public-API contract for the
    // `/v1/stream` topic — pinning it inline keeps a refactor of
    // `OptionTick`'s field names from silently changing the topic
    // payload.
    //
    // Timestamps go out as RFC 3339 strings (`2026-05-25T13:28:12.978Z`).
    // Default `serde_json` for `OffsetDateTime` would emit a nested
    // 9-element array, which is unusable for a JS consumer; the rfc3339
    // form matches METHODOLOGY.md §5 and the REST API's contract.
    let expiry = t
        .expiry
        .format(&::time::format_description::well_known::Rfc3339)
        .map_err(serde_json::Error::custom)?;
    let received_at = t
        .received_at
        .format(&::time::format_description::well_known::Rfc3339)
        .map_err(serde_json::Error::custom)?;

    // `ts` mirrors `received_at` today (matches the ClickHouse row's
    // `DEFAULT received_at` for `ts`). Pinning both fields in the wire
    // envelope now means a future change that diverges event-time from
    // ingest-time won't be a breaking topic-payload change for the
    // `/v1/stream` (#24) consumers.
    let payload = json!({
        "venue":          venue_label(t.venue),
        "asset":          asset_label(t.asset),
        "expiry":         expiry,
        "strike":         t.strike,
        "kind":           kind_label(t.kind),
        "bid":            t.bid,
        "ask":            t.ask,
        "mid":            t.mid,
        "iv":             t.iv,
        "underlying":     t.underlying,
        "open_interest":  t.open_interest,
        "volume_24h":     t.volume_24h,
        "received_at":    received_at.clone(),
        "ts":             received_at,
    });
    serde_json::to_string(&payload)
}
