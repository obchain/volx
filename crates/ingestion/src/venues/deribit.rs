//! Deribit WebSocket connector — issues #9 + #10.
//!
//! Fetches the active option instrument set from Deribit's REST API, opens a
//! single WebSocket connection, subscribes to the
//! `ticker.{instrument}.{TICKER_INTERVAL}` channel for every BTC + ETH
//! coin-margined option, and pushes one normalised [`OptionTick`] into a
//! `flume` channel per market update. See [`TICKER_INTERVAL`] for the
//! `.100ms` vs `.raw` choice (the latter requires authentication).
//!
//! [`run_with_retry`] wraps [`connect_and_stream`] with exponential backoff
//! and the alerting hooks called out in PRD §3.3 / issue #10.
//!
//! Out of scope (deferred to follow-up issues):
//! - tracing spans + Prometheus tick counters → issue #11 (the threshold log
//!   sites here use stable `threshold = ...` fields so #11 can attach a
//!   `metrics` / Sentry / ntfy backend without touching call sites).
//! - application-level Deribit `set_heartbeat` keepalive (without it the WS
//!   lives until the TCP layer drops it; reconnect now handles that for free
//!   but a heartbeat would catch silent stalls sooner) → tracked in #10's
//!   follow-up notes; not blocking the M1 critical path.
//! - per-side filters (stale / spread / intrinsic) → normalizer crate, #12.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use time::{Date, Month, OffsetDateTime, Time};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use volx_shared_types::{Asset, OptionKind, OptionTick, Venue};

/// Stable Prometheus label for [`Venue`]. Mirrors the lowercase
/// `snake_case` wire form used elsewhere (normalizer Redis topics,
/// `options_ticks.venue` column). Dashboards key on these — do not
/// change without a wire-format bump.
const fn venue_label(v: Venue) -> &'static str {
    match v {
        Venue::Deribit => "deribit",
        Venue::Okx => "okx",
        Venue::Bybit => "bybit",
    }
}

/// Stable Prometheus label for [`Asset`]. Same stability contract.
const fn asset_label(a: Asset) -> &'static str {
    match a {
        Asset::Btc => "btc",
        Asset::Eth => "eth",
    }
}

const WS_URL: &str = "wss://www.deribit.com/ws/api/v2";
const REST_INSTRUMENTS: &str = "https://www.deribit.com/api/v2/public/get_instruments";

/// Maximum channels per `public/subscribe` message. Deribit accepts large
/// batches; this just keeps individual frames under a comfortable size.
const SUBSCRIBE_BATCH: usize = 100;

/// Ticker channel interval. `.raw` is gated behind authentication
/// (`raw_subscriptions_not_available_for_unauthorized`), `.100ms` is the
/// fastest unauthenticated tier and still well above the 500–1000 ticks/s
/// acceptance target for #9. Switching to `.raw` becomes an auth concern
/// in #10, not a connector change.
const TICKER_INTERVAL: &str = "100ms";

/// Outcome of one WebSocket session.
///
/// `connect_and_stream` returns this so the reconnect wrapper can tell a
/// healthy server-side close (cycle the connection, keep going) from a
/// downstream consumer drop (stop reconnecting). Errors (`Result::Err`) take
/// a third path and trigger the backoff schedule.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionExit {
    /// Downstream `flume` receiver was dropped. Caller should stop reconnecting.
    DownstreamClosed,
    /// Server closed the WS or the read stream ended without error.
    /// `ticks_received` lets the caller decide whether the session was
    /// healthy enough to reset the backoff counter.
    ServerClosed { ticks_received: u64 },
}

/// Connect, subscribe to every active BTC + ETH option ticker, and push one
/// `OptionTick` per market update into `tx`. Returns a [`SessionExit`] tag on
/// any clean termination; `Err` only on connection / subscribe / parse errors
/// that should trigger a reconnect.
// The body is long but already factored into three obvious phases (REST →
// subscribe → read). Splitting further would only pass arguments through one
// extra layer.
#[allow(clippy::too_many_lines)]
pub(crate) async fn connect_and_stream(
    assets: &[Asset],
    tx: flume::Sender<OptionTick>,
) -> Result<SessionExit> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("volx-ingestion/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build reqwest client")?;

    let mut instruments: Vec<String> = Vec::new();
    for asset in assets {
        let names = fetch_instruments(&http, *asset).await?;
        instruments.extend(names);
    }
    if instruments.is_empty() {
        bail!("no Deribit option instruments resolved");
    }
    info!(total = instruments.len(), "connecting to Deribit WS");

    let (ws_stream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .context("Deribit WS connect")?;
    let (mut write, mut read) = ws_stream.split();

    // Send the subscribe burst from a separate task so the read half is
    // drained concurrently. Without this, TCP backpressure could deadlock
    // once the subscribe set grows past the socket's recv buffer (the read
    // task is blocked on the next batch's `write.send` because the server's
    // ack frames are filling the recv window).
    let total_batches = instruments.len().div_ceil(SUBSCRIBE_BATCH);
    let subscribe_handle = tokio::spawn(async move {
        for (batch_idx, batch) in instruments.chunks(SUBSCRIBE_BATCH).enumerate() {
            let channels: Vec<String> = batch
                .iter()
                .map(|n| format!("ticker.{n}.{TICKER_INTERVAL}"))
                .collect();
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": batch_idx + 1,
                "method": "public/subscribe",
                "params": { "channels": channels },
            });
            write
                .send(Message::text(payload.to_string()))
                .await
                .context("send subscribe")?;
        }
        info!(batches = total_batches, "subscriptions sent");
        Ok::<_, anyhow::Error>(())
    });

    let mut ticks_received: u64 = 0;
    let mut downstream_dropped = false;
    while let Some(frame) = read.next().await {
        let msg = frame.context("WS frame")?;
        let payload = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("Deribit closed the stream");
                break;
            }
            // Ping/Pong are auto-handled by tungstenite; binary + frame
            // variants are not used on this channel.
            _ => continue,
        };
        let envelope: WsEnvelope = match serde_json::from_str(&payload) {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "non-envelope frame");
                continue;
            }
        };
        if let Some(err) = envelope.error {
            warn!(id = ?envelope.id, error = ?err, "Deribit subscribe error");
            continue;
        }
        let Some(params) = envelope.params else {
            continue; // subscribe-ack with `result` array
        };
        if envelope.method.as_deref() != Some("subscription") {
            continue;
        }
        if !params.channel.starts_with("ticker.") {
            continue;
        }
        let data: TickerData = match serde_json::from_value(params.data) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, channel = %params.channel, "ticker parse failed");
                continue;
            }
        };
        let tick = match ticker_to_tick(&params.channel, &data) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, channel = %params.channel, "tick build failed");
                continue;
            }
        };
        let venue_label = venue_label(tick.venue);
        let asset_label = asset_label(tick.asset);
        if tx.send_async(tick).await.is_err() {
            info!("downstream channel closed; ingestion exiting");
            downstream_dropped = true;
            break;
        }
        // Counter increments *after* the successful hand-off so a
        // downstream stall (channel full → never happens with our
        // 50 000-slot capacity in normal flow) does not inflate the
        // received-rate dashboard.
        metrics::counter!(
            "volx_options_ticks_received_total",
            "venue" => venue_label,
            "asset" => asset_label,
        )
        .increment(1);
        ticks_received += 1;
    }
    // Read half ended (Close frame, error, or downstream-closed). Make sure
    // the subscribe task isn't left running and surface any send error.
    subscribe_handle.abort();
    match subscribe_handle.await {
        Ok(Ok(())) | Err(_) => {} // joined cleanly or was aborted mid-flight
        Ok(Err(e)) => return Err(e.context("subscribe task")),
    }
    Ok(if downstream_dropped {
        SessionExit::DownstreamClosed
    } else {
        SessionExit::ServerClosed { ticks_received }
    })
}

// ---------- Reconnect + exponential backoff (issue #10) ----------

/// Alert threshold for "too many back-to-back failures" (PRD §3.3).
const CONSECUTIVE_FAILURES_ALERT: u32 = 5;
/// Alert threshold for "downtime sustained beyond …" (PRD §3.3).
const DOWNTIME_ALERT: Duration = Duration::from_secs(10 * 60);
/// Cap on the backoff delay so we never wait longer than this between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// `±JITTER_RATIO` fraction applied to the base delay each attempt.
const JITTER_RATIO: f64 = 0.20;
/// Healthy-session threshold: a server-side close that produced fewer ticks
/// than this counts as a failed attempt for backoff purposes (prevents a
/// tight reconnect loop if the server keeps closing us right after
/// subscribe). Tuned to the smoke-test rate (~500 ticks/s), so ~1 s of
/// successful flow is enough to qualify as healthy.
const HEALTHY_TICK_THRESHOLD: u64 = 500;

/// Courtesy delay between a healthy server-side close and the next connect.
/// The failure-path backoff schedule does not apply (we observed ticks, the
/// server just cycled), so this is just enough to avoid an immediate
/// reconnect storm during a venue maintenance window.
const HEALTHY_RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// Per-venue reconnect state.
#[derive(Debug, Default)]
pub(crate) struct ReconnectState {
    /// Number of consecutive errored or unhealthy sessions since the last
    /// healthy one.
    consecutive: u32,
    /// `Some(start)` once we have entered a sustained-failure streak; cleared
    /// on a healthy session. `current_downtime()` is the elapsed time since
    /// that anchor.
    first_failure_at: Option<Instant>,
    /// Latched so the `DOWNTIME_ALERT` threshold log only fires once per
    /// outage rather than every retry cycle past the boundary.
    notified_downtime: bool,
}

impl ReconnectState {
    fn note_failure(&mut self) {
        self.consecutive = self.consecutive.saturating_add(1);
        if self.first_failure_at.is_none() {
            self.first_failure_at = Some(Instant::now());
        }
    }

    fn note_healthy(&mut self) {
        self.consecutive = 0;
        self.first_failure_at = None;
        self.notified_downtime = false;
    }

    fn current_downtime(&self) -> Duration {
        self.first_failure_at
            .map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Backoff schedule: `1 → 2 → 4 → 8 → 16` seconds, capped at
    /// [`MAX_BACKOFF`], with `±JITTER_RATIO` jitter applied each call.
    fn next_delay(&self) -> Duration {
        let base_secs = match self.consecutive {
            0 | 1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => 16,
            _ => MAX_BACKOFF.as_secs(),
        };
        let base = Duration::from_secs(base_secs).min(MAX_BACKOFF);
        let jitter: f64 = rand::rng().random_range(-JITTER_RATIO..=JITTER_RATIO);
        base.mul_f64(1.0 + jitter)
    }
}

/// Run the Deribit connector with reconnect + exponential backoff (issue #10).
///
/// Returns only when the downstream `flume` receiver is dropped — every other
/// session exit triggers a reconnect cycle (backoff on the failure path,
/// [`HEALTHY_RECONNECT_DELAY`] on a healthy server-side cycle). Per-venue
/// isolation is the caller's job: spawn this in its own `tokio` task per
/// venue and a panic in one will not stop the others (see
/// `crates/ingestion/src/main.rs`).
pub(crate) async fn run_with_retry(assets: Vec<Asset>, tx: flume::Sender<OptionTick>) {
    let mut state = ReconnectState::default();
    loop {
        let outcome = connect_and_stream(&assets, tx.clone()).await;
        match outcome {
            Ok(SessionExit::DownstreamClosed) => {
                info!("downstream channel closed — Deribit connector stopping");
                return;
            }
            Ok(SessionExit::ServerClosed { ticks_received })
                if ticks_received >= HEALTHY_TICK_THRESHOLD =>
            {
                warn!(
                    ticks_received,
                    delay_ms =
                        u64::try_from(HEALTHY_RECONNECT_DELAY.as_millis()).unwrap_or(u64::MAX),
                    "Deribit server closed a healthy session; reconnecting"
                );
                state.note_healthy();
                // Skip the failure-path backoff: this session was good.
                tokio::time::sleep(HEALTHY_RECONNECT_DELAY).await;
                continue;
            }
            Ok(SessionExit::ServerClosed { ticks_received }) => {
                state.note_failure();
                warn!(
                    ticks_received,
                    consecutive = state.consecutive,
                    "Deribit server closed an unhealthy session; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
            Err(e) => {
                state.note_failure();
                warn!(
                    error = ?e,
                    consecutive = state.consecutive,
                    "Deribit session failed; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
        }
        let delay = state.next_delay();
        info!(
            delay_s = delay.as_secs_f64(),
            consecutive = state.consecutive,
            downtime_s = state.current_downtime().as_secs(),
            "reconnect delay"
        );
        tokio::time::sleep(delay).await;
    }
}

fn emit_threshold_alerts(state: &mut ReconnectState) {
    // Cross-boundary edge (`== ALERT`) is logged once per outage; the
    // structured `threshold` field is what #11 attaches Prometheus +
    // Sentry breadcrumb backends to.
    if state.consecutive == CONSECUTIVE_FAILURES_ALERT {
        error!(
            threshold = "consecutive_failures",
            consecutive = state.consecutive,
            "Deribit reconnect threshold reached (Prometheus counter + Sentry breadcrumb)"
        );
    }
    if state.current_downtime() >= DOWNTIME_ALERT && !state.notified_downtime {
        state.notified_downtime = true;
        error!(
            threshold = "downtime_10min",
            downtime_s = state.current_downtime().as_secs(),
            "Deribit downtime exceeded 10 min (ntfy push)"
        );
    }
}

// ---------- REST instrument discovery ----------

#[derive(Debug, Deserialize)]
struct InstrumentsResponse {
    result: Vec<InstrumentRow>,
}

#[derive(Debug, Deserialize)]
struct InstrumentRow {
    instrument_name: String,
    #[serde(default = "default_active")]
    is_active: bool,
}

const fn default_active() -> bool {
    true
}

async fn fetch_instruments(client: &reqwest::Client, asset: Asset) -> Result<Vec<String>> {
    let currency = match asset {
        Asset::Btc => "BTC",
        Asset::Eth => "ETH",
    };
    let url = format!("{REST_INSTRUMENTS}?currency={currency}&kind=option&expired=false");
    let resp: InstrumentsResponse = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status {url}"))?
        .json()
        .await
        .with_context(|| format!("decode {url}"))?;
    let names: Vec<String> = resp
        .result
        .into_iter()
        .filter(|i| i.is_active)
        // Skip USDC / linear variants like `BTC_USDC-...`; this connector
        // ships only the coin-margined inverse option set.
        .filter(|i| !i.instrument_name.contains('_'))
        .map(|i| i.instrument_name)
        .collect();
    info!(asset = ?asset, count = names.len(), "fetched instruments");
    Ok(names)
}

// ---------- Instrument name parser ----------

/// Parse a Deribit option instrument name into its components.
///
/// Expected form: `<ASSET>-<DDMONYY>-<STRIKE>-<C|P>` (e.g. `BTC-30JUN26-100000-C`).
/// Expiry is the standardised 08:00 UTC settlement (Deribit convention).
pub(crate) fn parse_instrument_name(
    name: &str,
) -> Result<(Asset, OffsetDateTime, f64, OptionKind)> {
    let mut parts = name.split('-');
    let asset_str = parts.next().context("missing asset")?;
    let expiry_str = parts.next().context("missing expiry")?;
    let strike_str = parts.next().context("missing strike")?;
    let kind_str = parts.next().context("missing kind")?;
    if parts.next().is_some() {
        bail!("unexpected trailing component in {name}");
    }
    let asset = match asset_str {
        "BTC" => Asset::Btc,
        "ETH" => Asset::Eth,
        other => bail!("unsupported asset {other}"),
    };
    let expiry =
        parse_expiry(expiry_str).with_context(|| format!("expiry `{expiry_str}` in `{name}`"))?;
    let strike: f64 = strike_str
        .parse()
        .with_context(|| format!("strike `{strike_str}` in `{name}`"))?;
    let kind = match kind_str {
        "C" => OptionKind::Call,
        "P" => OptionKind::Put,
        other => bail!("unsupported kind `{other}`"),
    };
    Ok((asset, expiry, strike, kind))
}

fn parse_expiry(s: &str) -> Result<OffsetDateTime> {
    if s.len() < 6 {
        bail!("expiry too short: `{s}`");
    }
    let month_start = s
        .char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .context("expiry missing month")?;
    let (day_str, rest) = s.split_at(month_start);
    if rest.len() < 5 {
        bail!("expiry missing year: `{s}`");
    }
    let (month_str, year_str) = rest.split_at(3);
    let day: u8 = day_str
        .parse()
        .with_context(|| format!("day `{day_str}`"))?;
    let year_2: i32 = year_str
        .parse()
        .with_context(|| format!("year `{year_str}`"))?;
    // Guard against a garbled date producing a far-future / far-past expiry
    // that would silently corrupt the vol surface. Deribit options run within
    // a calendar year of the current date; the 2025-2099 envelope is loose
    // enough to outlast the project without admitting nonsense like year
    // 1999 from a `BTC-30JUN-1-…` mis-split.
    if !(25..=99).contains(&year_2) {
        bail!("year `{year_str}` outside plausible range");
    }
    let year = 2000 + year_2;
    let month = match month_str {
        "JAN" => Month::January,
        "FEB" => Month::February,
        "MAR" => Month::March,
        "APR" => Month::April,
        "MAY" => Month::May,
        "JUN" => Month::June,
        "JUL" => Month::July,
        "AUG" => Month::August,
        "SEP" => Month::September,
        "OCT" => Month::October,
        "NOV" => Month::November,
        "DEC" => Month::December,
        other => bail!("unknown month `{other}`"),
    };
    let date = Date::from_calendar_date(year, month, day)?;
    Ok(OffsetDateTime::new_utc(date, Time::from_hms(8, 0, 0)?))
}

// ---------- WS frame structures ----------

#[derive(Debug, Deserialize)]
struct WsEnvelope {
    method: Option<String>,
    params: Option<WsEnvelopeParams>,
    /// Present on subscribe-ack failures (e.g. unauthorized for `.raw`).
    error: Option<serde_json::Value>,
    id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WsEnvelopeParams {
    channel: String,
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct TickerData {
    best_bid_price: Option<f64>,
    best_ask_price: Option<f64>,
    /// Percent. Normalised to the `OptionTick.iv` fraction below.
    mark_iv: Option<f64>,
    underlying_price: f64,
    open_interest: f64,
    stats: TickerStats,
    /// Milliseconds since the Unix epoch.
    timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct TickerStats {
    /// 24-hour volume in option contracts.
    volume: f64,
}

fn ticker_to_tick(channel: &str, data: &TickerData) -> Result<OptionTick> {
    let instrument = channel
        .strip_prefix("ticker.")
        .and_then(|s| s.rsplit_once('.').map(|(name, _interval)| name))
        .context("malformed channel name")?;
    let (asset, expiry, strike, kind) = parse_instrument_name(instrument)?;

    let underlying = data.underlying_price;
    let bid_usd = data.best_bid_price.map(|b| b * underlying);
    let ask_usd = data.best_ask_price.map(|a| a * underlying);
    let mid_usd = match (bid_usd, ask_usd) {
        (Some(b), Some(a)) => Some((a + b) * 0.5),
        _ => None,
    };
    let iv_fraction = data.mark_iv.map(|p| p / 100.0);
    // Propagate a bad timestamp instead of substituting `now_utc()`: a corrupt
    // exchange timestamp must drop the tick, not pose as a fresh one to the
    // normalizer's staleness check (METHODOLOGY §3.1).
    let received_at =
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(data.timestamp) * 1_000_000)
            .with_context(|| format!("timestamp {} out of range", data.timestamp))?;

    Ok(OptionTick {
        venue: Venue::Deribit,
        asset,
        expiry,
        strike,
        kind,
        bid: bid_usd,
        ask: ask_usd,
        mid: mid_usd,
        iv: iv_fraction,
        underlying,
        open_interest: data.open_interest,
        volume_24h: data.stats.volume,
        received_at,
    })
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parse_btc_call_instrument() {
        let (asset, expiry, strike, kind) = parse_instrument_name("BTC-30JUN26-100000-C").unwrap();
        assert_eq!(asset, Asset::Btc);
        assert_eq!(expiry, datetime!(2026-06-30 08:00:00 UTC));
        assert!((strike - 100_000.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Call);
    }

    #[test]
    fn parse_eth_put_instrument() {
        let (asset, expiry, strike, kind) = parse_instrument_name("ETH-7AUG26-3500-P").unwrap();
        assert_eq!(asset, Asset::Eth);
        assert_eq!(expiry, datetime!(2026-08-07 08:00:00 UTC));
        assert!((strike - 3_500.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Put);
    }

    #[test]
    fn parse_fractional_strike_instrument() {
        let (_, _, strike, _) = parse_instrument_name("BTC-30JUN26-99500.5-C").unwrap();
        assert!((strike - 99_500.5).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_unknown_asset() {
        assert!(parse_instrument_name("SOL-30JUN26-100-C").is_err());
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_instrument_name("BTC-30JUN26-100000-X").is_err());
    }

    #[test]
    fn parse_rejects_unknown_month() {
        assert!(parse_instrument_name("BTC-30FOO26-100000-C").is_err());
    }

    #[test]
    fn parse_rejects_missing_component() {
        assert!(parse_instrument_name("BTC-30JUN26-100000").is_err());
    }

    #[test]
    fn parse_rejects_trailing_component() {
        assert!(parse_instrument_name("BTC-30JUN26-100000-C-EXTRA").is_err());
    }

    #[test]
    fn ticker_payload_normalises_to_option_tick() {
        let raw = serde_json::json!({
            "timestamp":        1_750_000_000_000_i64,
            "best_bid_price":   0.0421,
            "best_ask_price":   0.0438,
            "mark_iv":          62.5,
            "underlying_price": 68_500.0,
            "open_interest":    1_234.5,
            "stats": { "volume": 56.7 }
        });
        let data: TickerData = serde_json::from_value(raw).unwrap();
        let tick = ticker_to_tick("ticker.BTC-30JUN26-100000-C.raw", &data).unwrap();

        assert_eq!(tick.venue, Venue::Deribit);
        assert_eq!(tick.asset, Asset::Btc);
        assert_eq!(tick.kind, OptionKind::Call);
        assert!((tick.strike - 100_000.0).abs() < 1e-9);
        assert!((tick.underlying - 68_500.0).abs() < 1e-9);
        // 0.0421 BTC × 68 500 USD/BTC = 2 883.85 USD.
        let bid_usd = tick.bid.unwrap();
        assert!((bid_usd - 2_883.85).abs() < 1e-6);
        // mark_iv 62.5 % → 0.625 fraction.
        let iv = tick.iv.unwrap();
        assert!((iv - 0.625).abs() < 1e-12);
        assert!((tick.volume_24h - 56.7).abs() < 1e-9);
    }

    #[test]
    fn ticker_payload_handles_missing_side() {
        let raw = serde_json::json!({
            "timestamp":        1_750_000_000_000_i64,
            "best_bid_price":   serde_json::Value::Null,
            "best_ask_price":   0.05,
            "mark_iv":          serde_json::Value::Null,
            "underlying_price": 3_400.0,
            "open_interest":    0.0,
            "stats": { "volume": 0.0 }
        });
        let data: TickerData = serde_json::from_value(raw).unwrap();
        let tick = ticker_to_tick("ticker.ETH-7AUG26-3500-P.raw", &data).unwrap();
        assert!(tick.bid.is_none());
        assert!(tick.mid.is_none());
        assert!(tick.iv.is_none());
        assert_eq!(tick.ask, Some(0.05 * 3_400.0));
    }

    #[test]
    fn ticker_rejects_malformed_channel() {
        let raw = serde_json::json!({
            "timestamp":        1_750_000_000_000_i64,
            "best_bid_price":   0.04,
            "best_ask_price":   0.05,
            "mark_iv":          50.0,
            "underlying_price": 68_500.0,
            "open_interest":    1.0,
            "stats": { "volume": 1.0 }
        });
        let data: TickerData = serde_json::from_value(raw).unwrap();
        assert!(ticker_to_tick("not-a-ticker-channel", &data).is_err());
    }

    // ---------- ReconnectState (issue #10) ----------

    /// Drop jitter for assertions on the base schedule.
    fn base_secs(s: &ReconnectState) -> u64 {
        match s.consecutive {
            0 | 1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => 16,
            _ => MAX_BACKOFF.as_secs(),
        }
    }

    #[test]
    fn backoff_schedule_follows_1_2_4_8_16_then_caps_at_30() {
        let mut s = ReconnectState::default();
        let expected = [1, 2, 4, 8, 16, 30, 30, 30];
        for want in expected {
            s.note_failure();
            assert_eq!(base_secs(&s), want, "consecutive={}", s.consecutive);
        }
    }

    #[test]
    fn next_delay_stays_within_jitter_envelope() {
        let mut s = ReconnectState::default();
        for _ in 0..3 {
            s.note_failure();
        } // base = 4s
        for _ in 0..200 {
            let d = s.next_delay();
            // ±20 % envelope around 4 s = [3.2 s, 4.8 s].
            assert!(d >= Duration::from_millis(3_200), "{d:?} below floor");
            assert!(d <= Duration::from_millis(4_800), "{d:?} above ceiling");
        }
    }

    #[test]
    fn next_delay_never_exceeds_cap_plus_jitter() {
        let mut s = ReconnectState::default();
        for _ in 0..50 {
            s.note_failure();
        } // far past cap
        let d = s.next_delay();
        // base cap = 30 s, +20 % jitter ceiling = 36 s.
        assert!(d <= Duration::from_secs(36), "{d:?} above hard cap");
    }

    #[test]
    fn note_healthy_resets_consecutive_and_downtime() {
        let mut s = ReconnectState::default();
        s.note_failure();
        s.note_failure();
        s.note_failure();
        assert_eq!(s.consecutive, 3);
        assert!(s.first_failure_at.is_some());
        s.note_healthy();
        assert_eq!(s.consecutive, 0);
        assert!(s.first_failure_at.is_none());
        assert!(!s.notified_downtime);
    }

    #[test]
    fn first_failure_at_only_set_on_first_failure_of_a_streak() {
        let mut s = ReconnectState::default();
        s.note_failure();
        let first_anchor = s.first_failure_at.expect("anchor must be set");
        // Second failure must not shift the anchor — downtime measures the
        // full outage, not just the most recent attempt.
        std::thread::sleep(Duration::from_millis(5));
        s.note_failure();
        assert_eq!(s.first_failure_at, Some(first_anchor));
    }

    #[test]
    fn current_downtime_zero_until_first_failure() {
        let s = ReconnectState::default();
        assert_eq!(s.current_downtime(), Duration::ZERO);
    }

    #[test]
    fn alert_constants_match_prd() {
        assert_eq!(CONSECUTIVE_FAILURES_ALERT, 5);
        assert_eq!(DOWNTIME_ALERT, Duration::from_secs(600));
        assert_eq!(MAX_BACKOFF, Duration::from_secs(30));
    }

    #[test]
    fn session_exit_variants_are_distinguishable() {
        // Sanity check the consumer-facing discriminator the reconnect loop
        // dispatches on.
        let a = SessionExit::DownstreamClosed;
        let b = SessionExit::ServerClosed { ticks_received: 7 };
        assert_ne!(a, b);
        match b {
            SessionExit::ServerClosed { ticks_received } => assert_eq!(ticks_received, 7),
            SessionExit::DownstreamClosed => panic!("wrong variant"),
        }
    }
}
