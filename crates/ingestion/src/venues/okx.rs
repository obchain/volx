//! OKX WebSocket connector — issue #59.
//!
//! Subscribes to OKX's public `opt-summary` channel (per `instFamily`) and
//! emits one normalised [`OptionTick`] per option update. The `opt-summary`
//! channel carries IV (`markVol`), greeks, and a per-instrument forward
//! (`fwdPx`) — but **not** USD bid/ask prices. Those live on the per-instId
//! `tickers` channel which would require ~thousands of individual
//! subscriptions; out of scope for this PR.
//!
//! Practical effect: OKX ticks land in `options_ticks` with `iv` populated
//! and `bid` / `ask` / `mid` set to `None`. That's enough for the engine's
//! IV-surface fitter (it falls back to put-side IV when call is missing
//! and vice versa), and the cross-venue median blend (#61) is what will
//! eventually fuse this signal with Deribit's full mid surface.
//!
//! Wire shape pinned by a live probe against `wss://ws.okx.com:8443/ws/v5/public`
//! on 2026-05-27: all numeric fields ship as JSON strings (`markVol: "0.4172"`,
//! `ts: "1779889565813"`); subscribe args use `instFamily` (OKX deprecated
//! the older `uly` form — error code `64000`).
//!
//! Reuses the [`ReconnectState`] backoff curve from the Deribit module
//! (issue #10) since the schedule + alert thresholds are exchange-agnostic.
//! Lifting them into a shared helper is the next refactor once Bybit (#60)
//! lands.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use time::{Date, Month, OffsetDateTime, Time};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use volx_shared_types::{Asset, OptionKind, OptionTick, Venue};

const WS_URL: &str = "wss://ws.okx.com:8443/ws/v5/public";

/// Wall-clock deadline between successive WS frames. OKX leaves the
/// connection open on a subscribe error (code 64000 etc.) and will not
/// send anything further — without this timeout the read loop would
/// park indefinitely, bypassing the reconnect + alert machinery. Sized
/// at 60 s because `opt-summary` updates at ~1 Hz per instrument; a
/// genuine quiet period beyond a minute is itself a venue problem
/// worth a reconnect.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of one OKX session. Mirrors the Deribit module's discriminator
/// so the reconnect loop can decide between "cycle and continue" and
/// "stop" without inspecting the variant payload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionExit {
    /// Downstream `flume` receiver was dropped. Caller should stop reconnecting.
    DownstreamClosed,
    /// Server closed the WS or the read stream ended without error.
    /// `ticks_received` lets the caller decide whether the session was
    /// healthy enough to reset the backoff counter.
    ServerClosed { ticks_received: u64 },
}

/// Connect, subscribe to `opt-summary` for every BTC / ETH `instFamily`,
/// and push one `OptionTick` per option update into `tx`.
///
/// Returns a [`SessionExit`] tag on any clean termination; `Err` only on
/// connect / subscribe / parse errors that should trigger a reconnect.
#[allow(clippy::too_many_lines)]
pub(crate) async fn connect_and_stream(
    assets: &[Asset],
    tx: flume::Sender<OptionTick>,
) -> Result<SessionExit> {
    info!(assets = ?assets, "connecting to OKX WS");

    let (ws_stream, _) = tokio_tungstenite::connect_async(WS_URL)
        .await
        .context("OKX WS connect")?;
    let (mut write, mut read) = ws_stream.split();

    // Build one subscribe arg per `instFamily`. OKX's option universe is
    // already keyed by underlying family (`BTC-USD`, `ETH-USD`); two args
    // is enough to cover both asset classes without enumerating every
    // instrument the way Deribit's REST-driven flow does.
    let args: Vec<serde_json::Value> = assets
        .iter()
        .map(|asset| {
            let family = inst_family(*asset);
            serde_json::json!({ "channel": "opt-summary", "instFamily": family })
        })
        .collect();
    let subscribe = serde_json::json!({ "op": "subscribe", "args": args });
    write
        .send(Message::text(subscribe.to_string()))
        .await
        .context("send OKX subscribe")?;
    info!(args = assets.len(), "OKX subscribe sent");

    let mut ticks_received: u64 = 0;
    let mut downstream_dropped = false;
    loop {
        let frame = match tokio::time::timeout(READ_TIMEOUT, read.next()).await {
            Ok(Some(f)) => f,
            Ok(None) => break, // stream ended cleanly
            Err(_) => bail!(
                "OKX session read idle for {}s (subscribe rejected or feed stalled)",
                READ_TIMEOUT.as_secs()
            ),
        };
        let msg = frame.context("OKX WS frame")?;
        let payload = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("OKX closed the stream");
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

        // OKX returns `{"event": "subscribe" | "error", ...}` for control
        // frames and `{"arg": {...}, "data": [...]}` for push frames.
        // The subscribe-ack carries no `data`; an error has `event:"error"`.
        if let Some(event) = envelope.event.as_deref() {
            match event {
                "subscribe" => {
                    debug!(arg = ?envelope.arg, "OKX subscribe ack");
                }
                "error" => {
                    warn!(
                        code = envelope.code.as_deref().unwrap_or("?"),
                        msg = envelope.msg.as_deref().unwrap_or("?"),
                        "OKX subscribe error"
                    );
                }
                other => debug!(event = other, "OKX control frame"),
            }
            continue;
        }

        // Data frame.
        let Some(data) = envelope.data else {
            continue;
        };
        for row in data {
            if row.inst_type.as_deref() != Some("OPTION") {
                continue;
            }
            let tick = match summary_to_tick(&row) {
                Ok(t) => t,
                Err(e) => {
                    warn!(error = %e, inst_id = ?row.inst_id, "OKX tick build failed");
                    continue;
                }
            };
            let venue_label = tick.venue.label();
            let asset_label = tick.asset.label();
            if tx.send_async(tick).await.is_err() {
                info!("downstream channel closed; OKX ingestion exiting");
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
        if downstream_dropped {
            break;
        }
    }
    Ok(if downstream_dropped {
        SessionExit::DownstreamClosed
    } else {
        SessionExit::ServerClosed { ticks_received }
    })
}

// ---------- Reconnect + exponential backoff ----------

/// Alert threshold for "too many back-to-back failures" (PRD §3.3).
const CONSECUTIVE_FAILURES_ALERT: u32 = 5;
/// Alert threshold for "downtime sustained beyond …" (PRD §3.3).
const DOWNTIME_ALERT: Duration = Duration::from_secs(10 * 60);
/// Cap on the backoff delay so we never wait longer than this between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// `±JITTER_RATIO` fraction applied to the base delay each attempt.
const JITTER_RATIO: f64 = 0.20;
/// Healthy-session threshold. The Deribit threshold (500) targets the
/// ticker.100ms firehose; OKX's `opt-summary` updates per option-summary
/// recompute (~1 Hz per instrument), so 50 ticks ≈ 1 s of multi-strike
/// activity — a more permissive bar matching the channel's natural rate.
const HEALTHY_TICK_THRESHOLD: u64 = 50;
/// Courtesy delay between a healthy server-side close and the next connect.
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

/// Run the OKX connector with reconnect + exponential backoff. Returns
/// only when the downstream `flume` receiver is dropped.
pub(crate) async fn run_with_retry(assets: Vec<Asset>, tx: flume::Sender<OptionTick>) {
    // One-shot breadcrumb so dashboard authors querying `underlying`
    // across venues see why OKX values differ from Deribit's spot.
    // Fires once per process start, searchable via `note=fwdPx`.
    warn!(
        venue = "okx",
        note = "fwdPx",
        "OKX OptionTick.underlying carries the per-instrument forward (fwdPx), not the spot index — opt-summary does not publish spot"
    );

    let mut state = ReconnectState::default();
    loop {
        let outcome = connect_and_stream(&assets, tx.clone()).await;
        match outcome {
            Ok(SessionExit::DownstreamClosed) => {
                info!("downstream channel closed — OKX connector stopping");
                return;
            }
            Ok(SessionExit::ServerClosed { ticks_received })
                if ticks_received >= HEALTHY_TICK_THRESHOLD =>
            {
                warn!(
                    ticks_received,
                    delay_ms =
                        u64::try_from(HEALTHY_RECONNECT_DELAY.as_millis()).unwrap_or(u64::MAX),
                    "OKX server closed a healthy session; reconnecting"
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
                    "OKX server closed an unhealthy session; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
            Err(e) => {
                state.note_failure();
                warn!(
                    error = ?e,
                    consecutive = state.consecutive,
                    "OKX session failed; reconnecting"
                );
                emit_threshold_alerts(&mut state);
            }
        }
        let delay = state.next_delay();
        info!(
            delay_s = delay.as_secs_f64(),
            consecutive = state.consecutive,
            downtime_s = state.current_downtime().as_secs(),
            "OKX reconnect delay"
        );
        tokio::time::sleep(delay).await;
    }
}

fn emit_threshold_alerts(state: &mut ReconnectState) {
    if state.consecutive == CONSECUTIVE_FAILURES_ALERT {
        error!(
            venue = "okx",
            threshold = "consecutive_failures",
            consecutive = state.consecutive,
            "OKX reconnect threshold reached (Prometheus counter + Sentry breadcrumb)"
        );
    }
    if state.current_downtime() >= DOWNTIME_ALERT && !state.notified_downtime {
        state.notified_downtime = true;
        error!(
            venue = "okx",
            threshold = "downtime_10min",
            downtime_s = state.current_downtime().as_secs(),
            "OKX downtime exceeded 10 min (ntfy push)"
        );
    }
}

// ---------- WS frame structures ----------

#[derive(Debug, Deserialize)]
struct Envelope {
    /// Set on subscribe acks (`"subscribe"`) and on errors (`"error"`).
    event: Option<String>,
    /// Returned on the same frame as the subscribe-ack to confirm what
    /// was subscribed; we log it for debug but otherwise ignore.
    arg: Option<serde_json::Value>,
    /// Set on `event="error"` frames.
    code: Option<String>,
    /// Set on `event="error"` frames.
    msg: Option<String>,
    /// The push payload — populated only on data frames.
    data: Option<Vec<SummaryRow>>,
}

/// One row of `opt-summary` push data. OKX serializes every numeric field
/// as a JSON string (including timestamps); decode as `String` and parse
/// to `f64` / `i64` in [`summary_to_tick`].
#[derive(Debug, Deserialize)]
struct SummaryRow {
    #[serde(rename = "instType")]
    inst_type: Option<String>,
    #[serde(rename = "instId")]
    inst_id: Option<String>,
    /// Mark IV as decimal fraction (e.g. `"0.4172"`); already in the
    /// right scale, no `/100` needed.
    #[serde(rename = "markVol")]
    mark_vol: Option<String>,
    /// Per-instrument forward price — used as the `underlying` proxy
    /// since the spot/index price ships on a separate channel.
    #[serde(rename = "fwdPx")]
    fwd_px: Option<String>,
    /// Epoch milliseconds, serialized as a string.
    ts: Option<String>,
}

fn summary_to_tick(row: &SummaryRow) -> Result<OptionTick> {
    let inst_id = row.inst_id.as_deref().context("missing instId")?;
    let (asset, expiry, strike, kind) = parse_instrument_id(inst_id)?;
    let iv = row.mark_vol.as_deref().and_then(parse_decimal_opt);
    let fwd = row
        .fwd_px
        .as_deref()
        .and_then(parse_decimal_opt)
        .unwrap_or_else(|| {
            // `underlying` is `f64`, not `Option<f64>`, so NaN is the only
            // available sentinel. Log it once per occurrence so a quiet
            // wave of NaN underlyings is observable in Loki / Sentry
            // instead of silently propagating into the vol surface.
            debug!(inst_id, "fwdPx absent or non-finite; underlying set to NaN");
            f64::NAN
        });
    let ts_str = row.ts.as_deref().context("missing ts")?;
    let ts_ms: i64 = ts_str
        .parse()
        .with_context(|| format!("ts `{ts_str}` not i64"))?;
    let received_at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000)
        .with_context(|| format!("ts {ts_ms} out of range"))?;

    Ok(OptionTick {
        venue: Venue::Okx,
        asset,
        expiry,
        strike,
        kind,
        // `opt-summary` does not carry USD bid / ask; those live on the
        // per-instId `tickers` channel. Leaving these as `None` lets the
        // normalizer's mid-derivation skip OKX without pretending it has
        // a fresh mid. The cross-venue median blend (#61) will combine
        // the OKX IV with Deribit's mid surface.
        bid: None,
        ask: None,
        mid: None,
        iv,
        // `fwdPx` is the option-specific forward, not the spot index, but
        // it is the closest thing on this channel and the strip builder's
        // forward picker derives its own F from put-call parity anyway.
        // Surfaced so dashboards can sanity-check OKX vs Deribit forwards.
        underlying: fwd,
        open_interest: 0.0,
        volume_24h: 0.0,
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

const fn inst_family(asset: Asset) -> &'static str {
    match asset {
        Asset::Btc => "BTC-USD",
        Asset::Eth => "ETH-USD",
    }
}

// ---------- Instrument name parser ----------

/// Parse an OKX option instrument ID into its components.
///
/// Expected form: `<ASSET>-USD-<YYMMDD>-<STRIKE>-<C|P>`
/// (e.g. `BTC-USD-260731-64000-P`).
///
/// Expiry uses OKX's standardised 08:00 UTC settlement — matching the
/// Deribit convention so the engine's `time_to_expiry` rounds out the
/// same on both venues.
pub(crate) fn parse_instrument_id(name: &str) -> Result<(Asset, OffsetDateTime, f64, OptionKind)> {
    let mut parts = name.split('-');
    let asset_str = parts.next().context("missing asset")?;
    let quote = parts.next().context("missing quote currency")?;
    let date_str = parts.next().context("missing date")?;
    let strike_str = parts.next().context("missing strike")?;
    let kind_str = parts.next().context("missing kind")?;
    if parts.next().is_some() {
        bail!("unexpected trailing component in {name}");
    }
    if quote != "USD" {
        bail!("unsupported quote currency `{quote}` in {name}");
    }
    let asset = match asset_str {
        "BTC" => Asset::Btc,
        "ETH" => Asset::Eth,
        other => bail!("unsupported asset `{other}`"),
    };
    let expiry =
        parse_yymmdd(date_str).with_context(|| format!("date `{date_str}` in `{name}`"))?;
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

/// OKX dates are `YYMMDD` zero-padded. `260731` → 2026-07-31 08:00:00 UTC.
fn parse_yymmdd(s: &str) -> Result<OffsetDateTime> {
    // `is_ascii()` guard so the byte-indexed slicing below cannot panic
    // on a multi-byte UTF-8 sequence — exchange data is always ASCII
    // digits in practice, but a garbled frame should yield a clean
    // `Err`, not a panic.
    if s.len() != 6 || !s.is_ascii() {
        bail!("date `{s}` not exactly 6 ASCII digits");
    }
    let year_2: i32 = s[0..2]
        .parse()
        .with_context(|| format!("year `{}`", &s[0..2]))?;
    let month_n: u8 = s[2..4]
        .parse()
        .with_context(|| format!("month `{}`", &s[2..4]))?;
    let day: u8 = s[4..6]
        .parse()
        .with_context(|| format!("day `{}`", &s[4..6]))?;
    if !(25..=99).contains(&year_2) {
        bail!("year `{year_2}` outside plausible range");
    }
    let month = match month_n {
        1 => Month::January,
        2 => Month::February,
        3 => Month::March,
        4 => Month::April,
        5 => Month::May,
        6 => Month::June,
        7 => Month::July,
        8 => Month::August,
        9 => Month::September,
        10 => Month::October,
        11 => Month::November,
        12 => Month::December,
        _ => bail!("month `{month_n}` out of range"),
    };
    let year = 2000 + year_2;
    let date = Date::from_calendar_date(year, month, day)?;
    Ok(OffsetDateTime::new_utc(date, Time::from_hms(8, 0, 0)?))
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parse_btc_put_instrument() {
        let (asset, expiry, strike, kind) = parse_instrument_id("BTC-USD-260731-64000-P").unwrap();
        assert_eq!(asset, Asset::Btc);
        assert_eq!(expiry, datetime!(2026-07-31 08:00:00 UTC));
        assert!((strike - 64_000.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Put);
    }

    #[test]
    fn parse_eth_call_instrument() {
        let (asset, expiry, strike, kind) = parse_instrument_id("ETH-USD-260328-3500-C").unwrap();
        assert_eq!(asset, Asset::Eth);
        assert_eq!(expiry, datetime!(2026-03-28 08:00:00 UTC));
        assert!((strike - 3_500.0).abs() < 1e-9);
        assert_eq!(kind, OptionKind::Call);
    }

    #[test]
    fn parse_fractional_strike() {
        let (_, _, strike, _) = parse_instrument_id("BTC-USD-260731-64500.5-C").unwrap();
        assert!((strike - 64_500.5).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_non_usd_quote() {
        assert!(parse_instrument_id("BTC-USDT-260731-64000-C").is_err());
    }

    #[test]
    fn parse_rejects_unknown_asset() {
        assert!(parse_instrument_id("SOL-USD-260731-100-C").is_err());
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_instrument_id("BTC-USD-260731-64000-X").is_err());
    }

    #[test]
    fn parse_rejects_invalid_month() {
        assert!(parse_instrument_id("BTC-USD-261331-64000-C").is_err());
    }

    #[test]
    fn parse_rejects_short_date() {
        assert!(parse_instrument_id("BTC-USD-26073-64000-C").is_err());
    }

    #[test]
    fn parse_rejects_trailing_component() {
        assert!(parse_instrument_id("BTC-USD-260731-64000-C-EXTRA").is_err());
    }

    #[test]
    fn summary_row_normalises_to_option_tick() {
        // Fixture lifted from a live opt-summary frame captured 2026-05-27.
        let row = SummaryRow {
            inst_type: Some("OPTION".into()),
            inst_id: Some("BTC-USD-260731-64000-P".into()),
            mark_vol: Some("0.417167445".into()),
            fwd_px: Some("75156.2930395819".into()),
            ts: Some("1779889565813".into()),
        };
        let tick = summary_to_tick(&row).unwrap();
        assert_eq!(tick.venue, Venue::Okx);
        assert_eq!(tick.asset, Asset::Btc);
        assert_eq!(tick.kind, OptionKind::Put);
        assert!((tick.strike - 64_000.0).abs() < 1e-9);
        assert!((tick.underlying - 75_156.293_039_581_9).abs() < 1e-6);
        let iv = tick.iv.unwrap();
        assert!((iv - 0.417_167_445).abs() < 1e-12);
        // bid / ask / mid intentionally absent on `opt-summary`.
        assert!(tick.bid.is_none());
        assert!(tick.ask.is_none());
        assert!(tick.mid.is_none());
    }

    #[test]
    fn summary_row_with_empty_iv_decodes_to_none() {
        let row = SummaryRow {
            inst_type: Some("OPTION".into()),
            inst_id: Some("BTC-USD-260731-64000-P".into()),
            mark_vol: Some(String::new()),
            fwd_px: Some("75000".into()),
            ts: Some("1779889565813".into()),
        };
        let tick = summary_to_tick(&row).unwrap();
        assert!(tick.iv.is_none(), "empty markVol must yield None");
    }

    #[test]
    fn summary_row_rejects_non_numeric_ts() {
        let row = SummaryRow {
            inst_type: Some("OPTION".into()),
            inst_id: Some("BTC-USD-260731-64000-P".into()),
            mark_vol: Some("0.5".into()),
            fwd_px: Some("75000".into()),
            ts: Some("not-a-number".into()),
        };
        assert!(summary_to_tick(&row).is_err());
    }

    #[test]
    fn summary_row_rejects_missing_ts() {
        let row = SummaryRow {
            inst_type: Some("OPTION".into()),
            inst_id: Some("BTC-USD-260731-64000-P".into()),
            mark_vol: Some("0.5".into()),
            fwd_px: Some("75000".into()),
            ts: None,
        };
        let err = summary_to_tick(&row).unwrap_err();
        assert!(err.to_string().contains("missing ts"));
    }

    #[test]
    fn inst_family_maps_known_assets() {
        assert_eq!(inst_family(Asset::Btc), "BTC-USD");
        assert_eq!(inst_family(Asset::Eth), "ETH-USD");
    }

    #[test]
    fn parse_decimal_opt_handles_blank_and_non_finite() {
        assert_eq!(parse_decimal_opt(""), None);
        assert_eq!(parse_decimal_opt("inf"), None);
        assert_eq!(parse_decimal_opt("0.5"), Some(0.5));
    }

    // ---------- ReconnectState (shared schedule with Deribit) ----------

    #[test]
    fn backoff_schedule_caps_at_30s() {
        let mut s = ReconnectState::default();
        for _ in 0..10 {
            s.note_failure();
        }
        let d = s.next_delay();
        // 30 s + 20 % jitter ceiling.
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
