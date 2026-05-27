//! Bybit WebSocket connector — issue #60.
//!
//! Pulls the trading-status option set from Bybit's REST API per asset
//! (`/v5/market/instruments-info?category=option`), opens a single WS
//! connection to the public options endpoint, subscribes to the
//! `tickers.<symbol>` topic for every BTC + ETH symbol, and emits one
//! [`OptionTick`] per push.
//!
//! Unlike OKX's `opt-summary` (#59), Bybit's `tickers` channel carries
//! USD bid/ask, IV, mark, **and** index/underlying — everything the
//! engine needs in one frame. That makes Bybit ticks fully usable by
//! the strip builder without waiting for the cross-venue blend (#61).
//!
//! Wire shape captured by a live probe against
//! `wss://stream.bybit.com/v5/public/option` on 2026-05-27:
//!
//! ```json
//! {
//!   "topic": "tickers.BTC-26MAR27-78000-P-USDT",
//!   "ts":    1779891173028,
//!   "type":  "snapshot",
//!   "data":  { "symbol": "...", "bidPrice": "12575", "askPrice": "12975",
//!              "bidIv":   "0.4314", "askIv":   "0.4459",
//!              "markPrice": "12750.56", "markPriceIv": "0.4379",
//!              "indexPrice": "75007.4", "underlyingPrice": "77006.4",
//!              "openInterest": "0.17", "volume24h": "0", … }
//! }
//! ```
//!
//! Bybit serializes every numeric data-field as a JSON string (matches
//! OKX), but the envelope-level `ts` is an integer; the deserializer
//! reflects both. Symbol format is `<ASSET>-DDMONYY-<STRIKE>-<C|P>-USDT`
//! (5 parts).
//!
//! The reconnect schedule mirrors the OKX and Deribit modules verbatim;
//! the shared-helper refactor lands once this PR lands and there are
//! three call sites to consolidate.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use time::{Date, Month, OffsetDateTime, Time};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use volx_shared_types::{Asset, OptionKind, OptionTick, Venue};

const WS_URL: &str = "wss://stream.bybit.com/v5/public/option";
const REST_INSTRUMENTS: &str = "https://api.bybit.com/v5/market/instruments-info";

/// Bybit accepts up to 10 args per subscribe frame on v5 public WS;
/// pinning to a conservative batch size avoids the WSPING-induced
/// connection reset some clients observe on larger bursts.
///
/// Rate-budget note: Bybit v5 public WS imposes ~500 messages / 10 s
/// per connection. At ~1 000 symbols/2 assets = 100 subscribe frames,
/// the burst occupies ~20 % of the cap per window — comfortably safe.
/// A 5× symbol-count growth would still fit; revisit if Bybit's option
/// universe ever crosses ~5 000 strikes.
const SUBSCRIBE_BATCH: usize = 10;

/// Wall-clock deadline between successive frames. Bybit's ticker pushes
/// arrive ~1 Hz per active symbol; a quiet stretch beyond 60 s means
/// either the subscribe was rejected or the feed has stalled — either
/// way the right move is to bail out and let the reconnect loop take
/// over rather than park indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of one WebSocket session. Mirrors the OKX / Deribit
/// discriminator so the reconnect loop dispatch stays identical.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionExit {
    DownstreamClosed,
    ServerClosed { ticks_received: u64 },
}

/// Connect, fetch the BTC + ETH option universe via REST, subscribe to
/// every `tickers.<symbol>` topic in batches, and push one `OptionTick`
/// per market update.
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

    let mut symbols: Vec<String> = Vec::new();
    for asset in assets {
        let names = fetch_symbols(&http, *asset).await?;
        symbols.extend(names);
    }
    if symbols.is_empty() {
        bail!("no Bybit option symbols resolved");
    }
    info!(total = symbols.len(), "connecting to Bybit WS");

    let (ws_stream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .context("Bybit WS connect")?;
    let (mut write, mut read) = ws_stream.split();

    // Send subscribe batches from a sibling task so the read half drains
    // concurrently. Same TCP-backpressure rationale as Deribit's
    // subscribe sender.
    let total_batches = symbols.len().div_ceil(SUBSCRIBE_BATCH);
    let subscribe_handle = tokio::spawn(async move {
        for batch in symbols.chunks(SUBSCRIBE_BATCH) {
            let args: Vec<String> = batch.iter().map(|s| format!("tickers.{s}")).collect();
            let payload = serde_json::json!({ "op": "subscribe", "args": args });
            write
                .send(Message::text(payload.to_string()))
                .await
                .context("send Bybit subscribe")?;
        }
        info!(batches = total_batches, "Bybit subscriptions sent");
        Ok::<_, anyhow::Error>(())
    });

    let mut ticks_received: u64 = 0;
    let mut downstream_dropped = false;
    loop {
        let frame = match tokio::time::timeout(READ_TIMEOUT, read.next()).await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => bail!(
                "Bybit session read idle for {}s (subscribe rejected or feed stalled)",
                READ_TIMEOUT.as_secs()
            ),
        };
        let msg = frame.context("Bybit WS frame")?;
        let payload = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("Bybit closed the stream");
                break;
            }
            _ => continue,
        };
        let envelope: Envelope = match serde_json::from_str(&payload) {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "non-envelope frame");
                continue;
            }
        };

        // Bybit COMMAND_RESP frames are subscribe acks / errors. They
        // carry no ticker data and arrive once per `op:subscribe`. A
        // `success=false` means at least one topic was rejected; log
        // and keep reading because partial subscribes are still useful.
        if envelope.op.as_deref() == Some("subscribe")
            || envelope.r#type.as_deref() == Some("COMMAND_RESP")
        {
            if envelope.success == Some(false) {
                warn!(payload = %payload, "Bybit subscribe response not OK");
            } else {
                debug!("Bybit subscribe ack");
            }
            continue;
        }

        // Data frame.
        let Some(topic) = envelope.topic.as_deref() else {
            continue;
        };
        if !topic.starts_with("tickers.") {
            continue;
        }
        let Some(data) = envelope.data else {
            // A `tickers.*` push frame with no parseable `data` field
            // is a real anomaly (the COMMAND_RESP path already
            // continue'd above). Surface it so a schema migration
            // doesn't silently degrade the feed to zero.
            warn!(
                topic,
                "Bybit tickers push frame missing or unparseable data; possible schema change"
            );
            continue;
        };
        let Some(ts_ms) = envelope.ts else {
            warn!(topic, "Bybit push frame missing envelope ts; skipping");
            continue;
        };
        let tick = match data_to_tick(&data, ts_ms) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, symbol = ?data.symbol, "Bybit tick build failed");
                continue;
            }
        };
        let venue_label = tick.venue.label();
        let asset_label = tick.asset.label();
        if tx.send_async(tick).await.is_err() {
            info!("downstream channel closed; Bybit ingestion exiting");
            downstream_dropped = true;
            break;
        }
        metrics::counter!(
            "volx_options_ticks_received_total",
            "venue" => venue_label,
            "asset" => asset_label,
        )
        .increment(1);
        ticks_received += 1;
    }
    subscribe_handle.abort();
    match subscribe_handle.await {
        Ok(Ok(())) | Err(_) => {}
        Ok(Err(e)) => return Err(e.context("subscribe task")),
    }
    Ok(if downstream_dropped {
        SessionExit::DownstreamClosed
    } else {
        SessionExit::ServerClosed { ticks_received }
    })
}

// ---------- Reconnect + exponential backoff (mirrors OKX / Deribit) ----------

const CONSECUTIVE_FAILURES_ALERT: u32 = 5;
const DOWNTIME_ALERT: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const JITTER_RATIO: f64 = 0.20;
/// Bybit pushes at ~1 Hz per active symbol; 50 ticks ≈ ~1 s of multi-strike
/// activity, matching the OKX threshold.
const HEALTHY_TICK_THRESHOLD: u64 = 50;
const HEALTHY_RECONNECT_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub(crate) struct ReconnectState {
    consecutive: u32,
    first_failure_at: Option<Instant>,
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

pub(crate) async fn run_with_retry(assets: Vec<Asset>, tx: flume::Sender<OptionTick>) {
    let mut state = ReconnectState::default();
    loop {
        let outcome = connect_and_stream(&assets, tx.clone()).await;
        match outcome {
            Ok(SessionExit::DownstreamClosed) => {
                info!("downstream channel closed — Bybit connector stopping");
                return;
            }
            Ok(SessionExit::ServerClosed { ticks_received })
                if ticks_received >= HEALTHY_TICK_THRESHOLD =>
            {
                warn!(
                    ticks_received,
                    delay_ms =
                        u64::try_from(HEALTHY_RECONNECT_DELAY.as_millis()).unwrap_or(u64::MAX),
                    "Bybit server closed a healthy session; reconnecting"
                );
                state.note_healthy();
                tokio::time::sleep(HEALTHY_RECONNECT_DELAY).await;
                continue;
            }
            Ok(SessionExit::ServerClosed { ticks_received }) => {
                state.note_failure();
                warn!(
                    ticks_received,
                    consecutive = state.consecutive,
                    "Bybit server closed an unhealthy session; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
            Err(e) => {
                state.note_failure();
                warn!(
                    error = ?e,
                    consecutive = state.consecutive,
                    "Bybit session failed; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
        }
        let delay = state.next_delay();
        info!(
            delay_s = delay.as_secs_f64(),
            consecutive = state.consecutive,
            downtime_s = state.current_downtime().as_secs(),
            "Bybit reconnect delay"
        );
        tokio::time::sleep(delay).await;
    }
}

fn emit_threshold_alerts(state: &mut ReconnectState) {
    if state.consecutive == CONSECUTIVE_FAILURES_ALERT {
        error!(
            venue = "bybit",
            threshold = "consecutive_failures",
            consecutive = state.consecutive,
            "Bybit reconnect threshold reached (Prometheus counter + Sentry breadcrumb)"
        );
    }
    if state.current_downtime() >= DOWNTIME_ALERT && !state.notified_downtime {
        state.notified_downtime = true;
        error!(
            venue = "bybit",
            threshold = "downtime_10min",
            downtime_s = state.current_downtime().as_secs(),
            "Bybit downtime exceeded 10 min (ntfy push)"
        );
    }
}

// ---------- REST instrument discovery ----------

#[derive(Debug, Deserialize)]
struct InstrumentsResponse {
    #[serde(rename = "retCode")]
    ret_code: i32,
    result: Option<InstrumentsResult>,
}

#[derive(Debug, Deserialize)]
struct InstrumentsResult {
    list: Vec<InstrumentRow>,
    /// Pagination cursor — non-empty when the page was truncated.
    /// We follow it until empty to cover universes that grow past
    /// the per-page cap (1 000 today, soft cap on Bybit's side).
    #[serde(rename = "nextPageCursor", default)]
    next_page_cursor: String,
}

#[derive(Debug, Deserialize)]
struct InstrumentRow {
    symbol: String,
    status: String,
    #[serde(rename = "quoteCoin")]
    quote_coin: String,
}

async fn fetch_symbols(client: &reqwest::Client, asset: Asset) -> Result<Vec<String>> {
    let base_coin = match asset {
        Asset::Btc => "BTC",
        Asset::Eth => "ETH",
    };
    // Page through `nextPageCursor` so a future symbol count above the
    // per-page cap (1 000 today) is still fully fetched. Without this
    // the WS subscribe would silently cover only the first page.
    let mut names: Vec<String> = Vec::new();
    let mut cursor = String::new();
    let mut page_idx: u32 = 0;
    loop {
        let mut url = format!("{REST_INSTRUMENTS}?category=option&baseCoin={base_coin}&limit=1000");
        if !cursor.is_empty() {
            url.push_str("&cursor=");
            url.push_str(&urlencode(&cursor));
        }
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
        if resp.ret_code != 0 {
            bail!("Bybit instruments-info retCode={}", resp.ret_code);
        }
        let result = resp.result.context("missing result")?;
        names.extend(
            result
                .list
                .into_iter()
                .filter(|r| r.status == "Trading")
                .filter(|r| r.quote_coin == "USDT")
                .map(|r| r.symbol),
        );
        page_idx = page_idx.saturating_add(1);
        if result.next_page_cursor.is_empty() {
            break;
        }
        cursor = result.next_page_cursor;
        // Safety cap so a server bug producing a self-referential
        // cursor cannot lock us into an infinite REST loop.
        if page_idx >= 16 {
            warn!(
                asset = ?asset,
                pages = page_idx,
                "Bybit instruments-info pagination cap hit; stopping"
            );
            break;
        }
    }
    info!(asset = ?asset, count = names.len(), pages = page_idx, "Bybit symbols fetched");
    Ok(names)
}

/// Tiny URL-component encoder — enough to escape the Bybit
/// `nextPageCursor` string (which is opaque but may contain `+` or `/`).
/// Avoids pulling in a full `url` / `percent-encoding` crate dependency.
fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            // `write!` avoids the `format!`-to-temp-`String` allocation
            // clippy::format_push_string flags. Cannot fail on a `String`.
            let _ = write!(&mut out, "%{b:02X}");
        }
    }
    out
}

// ---------- WS frame structures ----------

#[derive(Debug, Deserialize)]
struct Envelope {
    /// Set on push frames (`"tickers.<symbol>"`).
    topic: Option<String>,
    /// Envelope-level timestamp (epoch ms, integer).
    ts: Option<i64>,
    /// `"snapshot"` / `"delta"` on push frames, `"COMMAND_RESP"` on
    /// subscribe acks.
    r#type: Option<String>,
    /// On `COMMAND_RESP` frames: confirms whether the subscribe succeeded.
    success: Option<bool>,
    /// Set on `COMMAND_RESP` frames as the echoed op name.
    op: Option<String>,
    /// On push frames: the ticker payload. On `COMMAND_RESP`: the topic
    /// list — but we deserialize the latter as a `Value` we don't read,
    /// so flatten to `Option<TickerData>` and let the `topic`-starts-with
    /// guard filter the latter out.
    data: Option<TickerData>,
}

/// `tickers.<symbol>` payload. Every numeric field ships as a JSON
/// string — same convention as OKX. Optional fields stay `Option<String>`
/// so the parser can distinguish "not present" from "present and 0".
#[derive(Debug, Deserialize)]
struct TickerData {
    symbol: Option<String>,
    #[serde(rename = "bidPrice")]
    bid_price: Option<String>,
    #[serde(rename = "askPrice")]
    ask_price: Option<String>,
    #[serde(rename = "bidIv")]
    bid_iv: Option<String>,
    #[serde(rename = "askIv")]
    ask_iv: Option<String>,
    /// Mark price — currently unread (we publish bid/ask mid), kept on
    /// the struct so a future #61 cross-venue sanity check can consume
    /// it without re-touching the wire shape.
    #[serde(rename = "markPrice")]
    #[allow(dead_code)]
    mark_price: Option<String>,
    #[serde(rename = "markPriceIv")]
    mark_price_iv: Option<String>,
    /// Spot / underlying — `indexPrice` is the venue's index, `underlyingPrice`
    /// is the per-instrument reference. They typically agree to within a few
    /// dollars; we prefer `indexPrice` and fall back to `underlyingPrice`.
    #[serde(rename = "indexPrice")]
    index_price: Option<String>,
    #[serde(rename = "underlyingPrice")]
    underlying_price: Option<String>,
    #[serde(rename = "openInterest")]
    open_interest: Option<String>,
    #[serde(rename = "volume24h")]
    volume_24h: Option<String>,
}

fn data_to_tick(d: &TickerData, ts_ms: i64) -> Result<OptionTick> {
    let symbol = d.symbol.as_deref().context("missing symbol")?;
    let (asset, expiry, strike, kind) = parse_symbol(symbol)?;

    let bid = d.bid_price.as_deref().and_then(parse_decimal_opt);
    let ask = d.ask_price.as_deref().and_then(parse_decimal_opt);
    let mid = match (bid, ask) {
        (Some(b), Some(a)) => Some((a + b) * 0.5),
        _ => None,
    };

    // IV preference: markPriceIv (venue's published mark IV) → bid/ask
    // midpoint when mark is blank. The midpoint requires BOTH sides
    // present by design: a one-sided IV on a deep-OTM contract is
    // typically a stale or stub quote, and feeding it into the IV
    // surface would skew the spline more than dropping the strike.
    // The engine's `pick_iv` already handles `iv = None` cleanly via
    // its call/put fallback, so dropping here is the conservative
    // choice. Revisit if cross-venue blend (#61) shows we are losing
    // useful signal on the wings.
    let iv = d
        .mark_price_iv
        .as_deref()
        .and_then(parse_decimal_opt)
        .or_else(|| {
            let b = d.bid_iv.as_deref().and_then(parse_decimal_opt)?;
            let a = d.ask_iv.as_deref().and_then(parse_decimal_opt)?;
            Some((a + b) * 0.5)
        });

    let underlying = d
        .index_price
        .as_deref()
        .and_then(parse_decimal_opt)
        .or_else(|| d.underlying_price.as_deref().and_then(parse_decimal_opt))
        .unwrap_or_else(|| {
            // Both `indexPrice` and `underlyingPrice` blank — observed
            // on deep-OTM contracts with no quotes. NaN is the only
            // sentinel `OptionTick.underlying: f64` admits; log so a
            // wave of NaN underlyings is visible in Loki / Sentry
            // rather than silently propagating into the vol surface.
            debug!(
                symbol,
                "Bybit indexPrice + underlyingPrice both absent; underlying = NaN"
            );
            f64::NAN
        });

    let open_interest = d
        .open_interest
        .as_deref()
        .and_then(parse_decimal_opt)
        .unwrap_or(0.0);
    let volume_24h = d
        .volume_24h
        .as_deref()
        .and_then(parse_decimal_opt)
        .unwrap_or(0.0);

    let received_at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000)
        .with_context(|| format!("ts {ts_ms} out of range"))?;
    // `mark_price` is intentionally not consumed today — we publish the
    // bid/ask mid as the canonical USD price. Kept on `TickerData` so
    // a future #61 follow-up can use it as a cross-venue sanity check.

    Ok(OptionTick {
        venue: Venue::Bybit,
        asset,
        expiry,
        strike,
        kind,
        bid,
        ask,
        mid,
        iv,
        underlying,
        open_interest,
        volume_24h,
        received_at,
    })
}

fn parse_decimal_opt(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let v: f64 = s.parse().ok()?;
    if v.is_finite() { Some(v) } else { None }
}

// ---------- Symbol parser ----------

/// Parse a Bybit option symbol.
///
/// Expected form: `<ASSET>-<DDMONYY>-<STRIKE>-<C|P>-USDT`
/// (e.g. `BTC-26MAR27-78000-P-USDT`). Expiry uses 08:00 UTC settlement
/// to align with Deribit / OKX, even though Bybit's actual delivery
/// time differs by a few hours — the engine only consumes
/// `time_to_expiry` rounded to the year fraction, well below
/// settlement-clock resolution.
pub(crate) fn parse_symbol(name: &str) -> Result<(Asset, OffsetDateTime, f64, OptionKind)> {
    let mut parts = name.split('-');
    let asset_str = parts.next().context("missing asset")?;
    let expiry_str = parts.next().context("missing expiry")?;
    let strike_str = parts.next().context("missing strike")?;
    let kind_str = parts.next().context("missing kind")?;
    let quote = parts.next().context("missing quote currency")?;
    if parts.next().is_some() {
        bail!("unexpected trailing component in `{name}`");
    }
    if quote != "USDT" {
        bail!("unsupported quote currency `{quote}` in `{name}`");
    }
    let asset = match asset_str {
        "BTC" => Asset::Btc,
        "ETH" => Asset::Eth,
        other => bail!("unsupported asset `{other}`"),
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

/// `DDMONYY` → 08:00 UTC settlement, matching the Deribit convention.
fn parse_expiry(s: &str) -> Result<OffsetDateTime> {
    if !s.is_ascii() {
        bail!("expiry `{s}` is not ASCII");
    }
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

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parse_btc_put_symbol() {
        let (asset, expiry, strike, kind) = parse_symbol("BTC-26MAR27-78000-P-USDT").unwrap();
        assert_eq!(asset, Asset::Btc);
        assert_eq!(expiry, datetime!(2027-03-26 08:00:00 UTC));
        assert!((strike - 78_000.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Put);
    }

    #[test]
    fn parse_eth_call_symbol() {
        let (asset, expiry, strike, kind) = parse_symbol("ETH-30JUN26-3500-C-USDT").unwrap();
        assert_eq!(asset, Asset::Eth);
        assert_eq!(expiry, datetime!(2026-06-30 08:00:00 UTC));
        assert!((strike - 3_500.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Call);
    }

    #[test]
    fn parse_rejects_non_usdt_quote() {
        assert!(parse_symbol("BTC-26MAR27-78000-P-USD").is_err());
    }

    #[test]
    fn parse_rejects_unknown_asset() {
        assert!(parse_symbol("SOL-26MAR27-78000-P-USDT").is_err());
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_symbol("BTC-26MAR27-78000-X-USDT").is_err());
    }

    #[test]
    fn parse_rejects_unknown_month() {
        assert!(parse_symbol("BTC-26FOO27-78000-P-USDT").is_err());
    }

    #[test]
    fn parse_rejects_short() {
        assert!(parse_symbol("BTC-26MAR27-78000-P").is_err());
    }

    #[test]
    fn parse_rejects_trailing_component() {
        assert!(parse_symbol("BTC-26MAR27-78000-P-USDT-EXTRA").is_err());
    }

    #[test]
    fn ticker_payload_normalises_to_option_tick() {
        // Fixture lifted from a real Bybit `tickers.BTC-26MAR27-78000-P-USDT`
        // snapshot frame captured 2026-05-27.
        let data = TickerData {
            symbol: Some("BTC-26MAR27-78000-P-USDT".into()),
            bid_price: Some("12575".into()),
            ask_price: Some("12975".into()),
            bid_iv: Some("0.4314".into()),
            ask_iv: Some("0.4459".into()),
            mark_price: Some("12750.56".into()),
            mark_price_iv: Some("0.4379".into()),
            index_price: Some("75007.40379205".into()),
            underlying_price: Some("77006.4".into()),
            open_interest: Some("0.17".into()),
            volume_24h: Some("0".into()),
        };
        let tick = data_to_tick(&data, 1_779_891_173_028).unwrap();
        assert_eq!(tick.venue, Venue::Bybit);
        assert_eq!(tick.asset, Asset::Btc);
        assert_eq!(tick.kind, OptionKind::Put);
        assert!((tick.strike - 78_000.0).abs() < 1e-9);
        // bid+ask USD mid.
        assert!((tick.bid.unwrap() - 12_575.0).abs() < 1e-9);
        assert!((tick.ask.unwrap() - 12_975.0).abs() < 1e-9);
        assert!((tick.mid.unwrap() - 12_775.0).abs() < 1e-9);
        // markPriceIv preferred over the bid/ask IV midpoint.
        assert!((tick.iv.unwrap() - 0.4379).abs() < 1e-12);
        // indexPrice wins over underlyingPrice when both present.
        assert!((tick.underlying - 75_007.403_792_05).abs() < 1e-6);
        assert!((tick.open_interest - 0.17).abs() < 1e-9);
    }

    #[test]
    fn iv_falls_back_to_bid_ask_midpoint_when_mark_iv_blank() {
        let data = TickerData {
            symbol: Some("BTC-26MAR27-78000-P-USDT".into()),
            bid_price: Some("12575".into()),
            ask_price: Some("12975".into()),
            bid_iv: Some("0.40".into()),
            ask_iv: Some("0.50".into()),
            mark_price: None,
            mark_price_iv: Some(String::new()), // empty → None
            index_price: Some("75000".into()),
            underlying_price: None,
            open_interest: None,
            volume_24h: None,
        };
        let tick = data_to_tick(&data, 1_779_891_173_028).unwrap();
        assert!((tick.iv.unwrap() - 0.45).abs() < 1e-12);
    }

    #[test]
    fn underlying_falls_back_to_underlying_price_when_index_blank() {
        let data = TickerData {
            symbol: Some("ETH-30JUN26-3500-C-USDT".into()),
            bid_price: None,
            ask_price: None,
            bid_iv: None,
            ask_iv: None,
            mark_price: None,
            mark_price_iv: Some("0.6".into()),
            index_price: Some(String::new()),
            underlying_price: Some("3450".into()),
            open_interest: None,
            volume_24h: None,
        };
        let tick = data_to_tick(&data, 1_779_891_173_028).unwrap();
        assert!((tick.underlying - 3_450.0).abs() < 1e-9);
    }

    #[test]
    fn missing_bid_or_ask_yields_no_mid() {
        let data = TickerData {
            symbol: Some("BTC-26MAR27-78000-P-USDT".into()),
            bid_price: None,
            ask_price: Some("100".into()),
            bid_iv: None,
            ask_iv: None,
            mark_price: None,
            mark_price_iv: Some("0.5".into()),
            index_price: Some("75000".into()),
            underlying_price: None,
            open_interest: None,
            volume_24h: None,
        };
        let tick = data_to_tick(&data, 1_779_891_173_028).unwrap();
        assert!(tick.bid.is_none());
        assert!(tick.mid.is_none());
        assert!((tick.ask.unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn data_to_tick_rejects_missing_symbol() {
        let data = TickerData {
            symbol: None,
            bid_price: None,
            ask_price: None,
            bid_iv: None,
            ask_iv: None,
            mark_price: None,
            mark_price_iv: None,
            index_price: None,
            underlying_price: None,
            open_interest: None,
            volume_24h: None,
        };
        assert!(data_to_tick(&data, 0).is_err());
    }

    #[test]
    fn parse_decimal_opt_handles_blank_and_non_finite() {
        assert_eq!(parse_decimal_opt(""), None);
        assert_eq!(parse_decimal_opt("nan"), None);
        assert_eq!(parse_decimal_opt("inf"), None);
        assert_eq!(parse_decimal_opt("1.5"), Some(1.5));
    }

    // ---------- ReconnectState ----------

    #[test]
    fn backoff_schedule_caps_at_30s() {
        let mut s = ReconnectState::default();
        for _ in 0..10 {
            s.note_failure();
        }
        let d = s.next_delay();
        assert!(d <= Duration::from_secs(36));
    }

    #[test]
    fn note_healthy_resets_state() {
        let mut s = ReconnectState::default();
        s.note_failure();
        s.note_failure();
        s.note_healthy();
        assert_eq!(s.consecutive, 0);
        assert!(s.first_failure_at.is_none());
    }

    #[test]
    fn session_exit_variants_distinguishable() {
        let a = SessionExit::DownstreamClosed;
        let b = SessionExit::ServerClosed { ticks_received: 0 };
        assert_ne!(a, b);
    }
}
