//! Chain assembler — turns the per-tick `options_ticks` table into
//! per-expiry [`ExpiryChain`] snapshots the strip builder can consume
//! (issue #20).
//!
//! Reads the **latest** tick per `(asset, expiry, strike, kind)` within
//! a freshness window (default 30 s) directly from `ClickHouse`. The
//! engine deliberately reads the system of record rather than the
//! Redis pubsub firehose so a recoverable snapshot lands every 60 s
//! even if the normalizer's Redis sink is degraded.
//!
//! Per-asset snapshots are returned as a `HashMap<expiry, ChainLeg[]>`
//! so the §4.1 expiry picker (in `snapshot.rs`) can pick near + next
//! without re-scanning.
//!
//! ## Year-fraction convention
//!
//! `time_to_expiry` is computed as `(expiry − ts) / (365 · 86400) s`.
//! `N_365` from `METHODOLOGY.md` §4.6 = 525 600 minutes = 31 536 000 s.
//! Using 365 (not 365.25) matches the spec; downstream `Minutes` math
//! in `interpolate.rs` is consistent.

use std::collections::HashMap;

use clickhouse::Client;
use serde::Deserialize;
use time::OffsetDateTime;
use tracing::{debug, warn};
use volx_shared_types::ids::Venue;
use volx_shared_types::units::Years;
use volx_shared_types::{Asset, OptionKind};

use crate::strip::{ChainLeg, ExpiryChain};

/// Seconds in a year per `METHODOLOGY.md` §4.6 (`N_365 = 365·1440·60`).
const SECONDS_PER_YEAR: f64 = 365.0 * 24.0 * 60.0 * 60.0;

/// Freshness window for the latest-tick query. Ticks older than this are
/// excluded — staleness in the source feed should not silently feed the
/// engine a 5-minute-old quote.
///
/// 30 s is `6×` the normalizer's `max_age_secs` default (5 s). A larger
/// window than that would let a feed outage masquerade as a live snapshot.
pub const SNAPSHOT_FRESHNESS_SECS: u32 = 30;

/// Wire row from the `argMax(...) GROUP BY ...` query below.
#[derive(Debug, Deserialize, clickhouse::Row)]
struct ChainRow {
    venue: String,
    asset: String,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    expiry: OffsetDateTime,
    strike: f64,
    kind: String,
    bid: Option<f64>,
    ask: Option<f64>,
    mid: Option<f64>,
    iv: Option<f64>,
    underlying: f64,
    /// Newest `ts` in this `(venue, asset, expiry, strike, kind)`
    /// group — feeds the per-venue freshness signal that backs
    /// `confidence::ConfidenceInputs::max_quote_age` (issue #62).
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    latest_ts: OffsetDateTime,
}

/// Why a chain build can fail.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("clickhouse error: {0}")]
    ClickHouse(#[from] clickhouse::error::Error),
}

/// Per-expiry chains keyed by `(asset, expiry)`. The expiry-picker step
/// in `snapshot.rs` consumes one `Asset` at a time, so the outer map
/// keys on `Asset` and the inner map on `OffsetDateTime`.
pub type AssetChains = HashMap<Asset, HashMap<OffsetDateTime, ExpiryChain>>;

/// One venue's chain data plus the per-venue freshness signal.
/// `latest_ts` is the newest `options_ticks.ts` observed across every
/// `(asset, expiry, strike, kind)` row this venue contributed — used
/// by `confidence::score` to demote venues whose feed has gone quiet
/// (issue #62, `confidence::ConfidenceInputs::max_quote_age`).
#[derive(Debug, Clone)]
pub struct VenueChains {
    pub assets: AssetChains,
    pub latest_ts: OffsetDateTime,
}

impl Default for VenueChains {
    /// Empty assets + `latest_ts = UNIX_EPOCH`. The epoch sentinel is
    /// the same one `assemble_chains` starts with before folding
    /// rows in; production paths overwrite it on the first row.
    /// Test helpers that build chains manually can set it directly.
    fn default() -> Self {
        Self {
            assets: AssetChains::default(),
            latest_ts: OffsetDateTime::UNIX_EPOCH,
        }
    }
}

/// Per-venue, per-asset chains. Outer key is the venue the rows came
/// from; the inner [`AssetChains`] follows the §4.1 per-venue picker
/// (issue #61). One venue's chains never share a strip with another's
/// — folding two venues' mid-quotes into one chain would silently
/// mix legs whose bid/ask come from different order books and the
/// strip builder's §4.2 forward picker would get a chain whose call
/// and put legs disagree on the underlying.
pub type MultiVenueChains = HashMap<Venue, VenueChains>;

/// Pull the latest tick per `(asset, expiry, strike, kind)` from
/// `ClickHouse` and assemble per-expiry chains.
///
/// `now` is the snapshot timestamp — used for the freshness filter and
/// for the `time_to_expiry` computation. The scheduler (#20) passes a
/// fresh `OffsetDateTime::now_utc()` per 60-second tick.
///
/// # Errors
///
/// Returns `Err` only on a `ClickHouse` driver / network failure. An
/// empty result (no ticks in the window) is a valid empty chain, not
/// an error — `snapshot.rs` decides whether to publish or skip.
///
/// # Panics
///
/// Does not panic; the `unwrap_or(i64::MAX)` fallback on the cutoff
/// conversion produces an *empty* result rather than a full-table
/// scan if the timestamp ever overflows `i64` (year ≈ 292,000,000 —
/// not reachable in any sane clock, but the right fallback is "skip
/// the tick" not "rescan a year of history").
pub async fn fetch_chains(
    client: &Client,
    now: OffsetDateTime,
) -> Result<MultiVenueChains, ChainError> {
    let cutoff_ms = i64::try_from(
        now.unix_timestamp_nanos() / 1_000_000 - i128::from(SNAPSHOT_FRESHNESS_SECS) * 1000,
    )
    .unwrap_or(i64::MAX);

    // `argMax(field, ts)` picks the field value at the row with the
    // largest `ts` in the group — i.e. the *latest* observation per
    // instrument. The `ts >= cutoff` filter drops stale rows so a
    // dead feed doesn't masquerade as a live snapshot.
    //
    // `venue` joins the GROUP BY so each venue's order book stays
    // its own strip — folding two venues' mid-quotes into one
    // `argMax(mid, ts)` would silently mix legs whose bid/ask come
    // from different books and the §4.2 forward picker would get a
    // chain whose call and put legs disagree on the underlying.
    // The downstream snapshot orchestrator (#61) blends per-venue
    // BVOLs with a median policy instead.
    let query = "
        SELECT
            venue,
            asset,
            expiry,
            strike,
            kind,
            argMax(bid, ts)        AS bid,
            argMax(ask, ts)        AS ask,
            argMax(mid, ts)        AS mid,
            argMax(iv,  ts)        AS iv,
            argMax(underlying, ts) AS underlying,
            max(ts)                AS latest_ts
        FROM volx.options_ticks
        WHERE ts >= fromUnixTimestamp64Milli(?)
        GROUP BY venue, asset, expiry, strike, kind
    ";

    let rows: Vec<ChainRow> = client
        .query(query)
        .bind(cutoff_ms)
        .fetch_all::<ChainRow>()
        .await?;

    debug!(
        rows = rows.len(),
        cutoff_ms, "fetched latest ticks for chain build"
    );

    Ok(assemble_chains(rows, now))
}

/// Fold a flat list of [`ChainRow`]s into per-venue, per-asset, per-expiry chains.
///
/// Pulled out of [`fetch_chains`] so the row-→-tree logic can be tested
/// without a `ClickHouse` connection. Unknown venues / assets / kinds and
/// non-finite strikes are dropped silently (the warn-log path lives
/// here too, so the test surface stays honest).
fn assemble_chains(rows: Vec<ChainRow>, now: OffsetDateTime) -> MultiVenueChains {
    let mut out: MultiVenueChains = HashMap::new();
    for row in rows {
        let Some(venue) = parse_venue(&row.venue) else {
            warn!(venue = %row.venue, "unknown venue in options_ticks; skipping");
            continue;
        };
        let Some(asset) = parse_asset(&row.asset) else {
            warn!(asset = %row.asset, "unknown asset in options_ticks; skipping");
            continue;
        };
        let Some(kind) = parse_kind(&row.kind) else {
            warn!(kind = %row.kind, "unknown option kind; skipping");
            continue;
        };
        if !row.strike.is_finite() || row.strike <= 0.0 {
            continue;
        }

        // Per-venue freshness: max across every contributing row.
        // Initial sentinel is `OffsetDateTime::UNIX_EPOCH` so the
        // first row's `latest_ts` always wins. Confidence pulls
        // `now - latest_ts` to compute the per-venue quote age.
        let per_venue = out.entry(venue).or_insert_with(|| VenueChains {
            assets: HashMap::new(),
            latest_ts: OffsetDateTime::UNIX_EPOCH,
        });
        if row.latest_ts > per_venue.latest_ts {
            per_venue.latest_ts = row.latest_ts;
        }
        let per_asset = per_venue.assets.entry(asset).or_default();
        let chain = per_asset.entry(row.expiry).or_insert_with(|| ExpiryChain {
            time_to_expiry: year_fraction(now, row.expiry),
            legs: Vec::new(),
        });

        // The `argMax` query yields one row per `(asset, expiry, strike,
        // kind)`. Fold call + put rows for the same strike into a
        // single `ChainLeg`.
        let leg = if let Some(existing) = chain
            .legs
            .iter_mut()
            .find(|l| (l.strike - row.strike).abs() < f64::EPSILON)
        {
            existing
        } else {
            chain.legs.push(ChainLeg {
                strike: row.strike,
                ..ChainLeg::default()
            });
            chain.legs.last_mut().expect("just pushed")
        };

        // `mid` may be null in the source (normalizer dropped one side
        // of a quote); the strip builder is OK with `None` and the
        // §4.2 forward picker filters strikes where either leg is
        // missing.
        match kind {
            OptionKind::Call => {
                leg.call_mid_usd = row.mid.filter(|v| v.is_finite());
                leg.call_iv = row.iv.filter(|v| v.is_finite());
            }
            OptionKind::Put => {
                leg.put_mid_usd = row.mid.filter(|v| v.is_finite());
                leg.put_iv = row.iv.filter(|v| v.is_finite());
            }
        }

        let _ = (row.bid, row.ask, row.underlying); // currently unused at chain level; reserved for future filters
    }

    out
}

fn parse_venue(s: &str) -> Option<Venue> {
    // Mirror of `Venue::label()` — the normalizer writes these
    // lowercase strings into `options_ticks.venue`.
    match s {
        "deribit" => Some(Venue::Deribit),
        "okx" => Some(Venue::Okx),
        "bybit" => Some(Venue::Bybit),
        _ => None,
    }
}

fn parse_asset(s: &str) -> Option<Asset> {
    match s {
        "btc" => Some(Asset::Btc),
        "eth" => Some(Asset::Eth),
        _ => None,
    }
}

fn parse_kind(s: &str) -> Option<OptionKind> {
    match s {
        "call" => Some(OptionKind::Call),
        "put" => Some(OptionKind::Put),
        _ => None,
    }
}

/// Year-fraction between `now` and `expiry` per `METHODOLOGY.md` §4.6
/// `N_365` convention (365 days, not 365.25).
#[must_use]
pub fn year_fraction(now: OffsetDateTime, expiry: OffsetDateTime) -> Years {
    let secs = (expiry - now).as_seconds_f64();
    Years(secs / SECONDS_PER_YEAR)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn year_fraction_30_days_is_30_over_365() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let expiry = now + time::Duration::days(30);
        let yf = year_fraction(now, expiry);
        assert!((yf.0 - 30.0 / 365.0).abs() < 1e-12, "got {}", yf.0);
    }

    #[test]
    fn year_fraction_one_year_is_one() {
        let now = datetime!(2026-01-01 00:00:00 UTC);
        let expiry = datetime!(2027-01-01 00:00:00 UTC);
        let yf = year_fraction(now, expiry);
        // 365 days exactly per the §4.6 convention.
        assert!((yf.0 - 1.0).abs() < 1e-12, "got {}", yf.0);
    }

    #[test]
    fn year_fraction_past_expiry_is_negative() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let expiry = now - time::Duration::days(1);
        let yf = year_fraction(now, expiry);
        assert!(yf.0 < 0.0);
    }

    #[test]
    fn parse_asset_known_variants() {
        assert_eq!(parse_asset("btc"), Some(Asset::Btc));
        assert_eq!(parse_asset("eth"), Some(Asset::Eth));
        assert_eq!(parse_asset("BTC"), None); // case-sensitive — normalizer writes lowercase
        assert_eq!(parse_asset("doge"), None);
    }

    #[test]
    fn parse_kind_known_variants() {
        assert_eq!(parse_kind("call"), Some(OptionKind::Call));
        assert_eq!(parse_kind("put"), Some(OptionKind::Put));
        assert_eq!(parse_kind("straddle"), None);
    }

    // ------- assemble_chains tests ------------------------------------

    fn row(
        venue: &str,
        asset: &str,
        expiry: OffsetDateTime,
        strike: f64,
        kind: &str,
        mid: Option<f64>,
        iv: Option<f64>,
    ) -> ChainRow {
        ChainRow {
            venue: venue.to_string(),
            asset: asset.to_string(),
            expiry,
            strike,
            kind: kind.to_string(),
            bid: None,
            ask: None,
            mid,
            iv,
            underlying: 100_000.0,
            // Most tests don't care about freshness — peg latest_ts
            // to `now` (datetime!(2026-05-25 …)) so the per-venue
            // `latest_ts` lands at a deterministic value the tests
            // can ignore. Freshness-specific tests build their own
            // rows.
            latest_ts: time::macros::datetime!(2026-05-25 00:00:00 UTC),
        }
    }

    #[test]
    fn parse_venue_known_variants() {
        assert_eq!(parse_venue("deribit"), Some(Venue::Deribit));
        assert_eq!(parse_venue("okx"), Some(Venue::Okx));
        assert_eq!(parse_venue("bybit"), Some(Venue::Bybit));
        assert_eq!(parse_venue("DERIBIT"), None); // case-sensitive — normalizer writes lowercase
        assert_eq!(parse_venue("kraken"), None);
    }

    #[test]
    fn assemble_drops_unknown_venue() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row("deribit", "btc", exp, 100.0, "call", Some(1.0), Some(0.5)),
            row("kraken", "btc", exp, 100.0, "call", Some(1.0), Some(0.5)),
        ];
        let out = assemble_chains(rows, now);
        assert!(out.contains_key(&Venue::Deribit));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn assemble_drops_unknown_asset() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row("deribit", "btc", exp, 100.0, "call", Some(1.0), Some(0.5)),
            row("deribit", "doge", exp, 100.0, "call", Some(1.0), Some(0.5)),
        ];
        let out = assemble_chains(rows, now);
        let by_asset = &out[&Venue::Deribit].assets;
        assert!(by_asset.contains_key(&Asset::Btc));
        assert_eq!(by_asset.len(), 1);
    }

    #[test]
    fn assemble_drops_unknown_kind() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row("deribit", "btc", exp, 100.0, "call", Some(1.0), Some(0.5)),
            row(
                "deribit",
                "btc",
                exp,
                100.0,
                "straddle",
                Some(1.0),
                Some(0.5),
            ),
        ];
        let out = assemble_chains(rows, now);
        let chain = &out[&Venue::Deribit].assets[&Asset::Btc][&exp];
        assert_eq!(chain.legs.len(), 1);
        assert!(chain.legs[0].call_mid_usd.is_some());
        assert!(chain.legs[0].put_mid_usd.is_none());
    }

    #[test]
    fn assemble_drops_non_finite_or_non_positive_strike() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row(
                "deribit",
                "btc",
                exp,
                f64::NAN,
                "call",
                Some(1.0),
                Some(0.5),
            ),
            row(
                "deribit",
                "btc",
                exp,
                f64::INFINITY,
                "call",
                Some(1.0),
                Some(0.5),
            ),
            row("deribit", "btc", exp, -50.0, "call", Some(1.0), Some(0.5)),
            row("deribit", "btc", exp, 0.0, "call", Some(1.0), Some(0.5)),
            row("deribit", "btc", exp, 100.0, "call", Some(1.0), Some(0.5)),
        ];
        let out = assemble_chains(rows, now);
        let chain = &out[&Venue::Deribit].assets[&Asset::Btc][&exp];
        assert_eq!(chain.legs.len(), 1, "only the K=100 row should survive");
        assert_eq!(chain.legs[0].strike, 100.0);
    }

    #[test]
    fn assemble_folds_call_and_put_into_one_leg() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row("deribit", "btc", exp, 100.0, "call", Some(5.0), Some(0.5)),
            row("deribit", "btc", exp, 100.0, "put", Some(4.0), Some(0.55)),
        ];
        let out = assemble_chains(rows, now);
        let chain = &out[&Venue::Deribit].assets[&Asset::Btc][&exp];
        assert_eq!(chain.legs.len(), 1);
        assert_eq!(chain.legs[0].strike, 100.0);
        assert_eq!(chain.legs[0].call_mid_usd, Some(5.0));
        assert_eq!(chain.legs[0].put_mid_usd, Some(4.0));
        assert_eq!(chain.legs[0].call_iv, Some(0.5));
        assert_eq!(chain.legs[0].put_iv, Some(0.55));
    }

    #[test]
    fn assemble_isolates_btc_and_eth() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row(
                "deribit",
                "btc",
                exp,
                100_000.0,
                "call",
                Some(5.0),
                Some(0.5),
            ),
            row("deribit", "eth", exp, 3_000.0, "call", Some(2.0), Some(0.6)),
        ];
        let out = assemble_chains(rows, now);
        let by_asset = &out[&Venue::Deribit].assets;
        assert_eq!(by_asset.len(), 2);
        assert_eq!(by_asset[&Asset::Btc][&exp].legs[0].strike, 100_000.0);
        assert_eq!(by_asset[&Asset::Eth][&exp].legs[0].strike, 3_000.0);
    }

    #[test]
    fn assemble_isolates_venues() {
        // Same asset + expiry + strike across two venues: each
        // venue keeps its own ChainLeg, so a fat-finger quote on
        // one venue cannot contaminate the other's strip.
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row(
                "deribit",
                "btc",
                exp,
                100_000.0,
                "call",
                Some(5.0),
                Some(0.50),
            ),
            row("okx", "btc", exp, 100_000.0, "call", Some(5.1), Some(0.51)),
            row(
                "bybit",
                "btc",
                exp,
                100_000.0,
                "call",
                Some(4.9),
                Some(0.49),
            ),
        ];
        let out = assemble_chains(rows, now);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[&Venue::Deribit].assets[&Asset::Btc][&exp].legs[0].call_mid_usd,
            Some(5.0)
        );
        assert_eq!(
            out[&Venue::Okx].assets[&Asset::Btc][&exp].legs[0].call_mid_usd,
            Some(5.1)
        );
        assert_eq!(
            out[&Venue::Bybit].assets[&Asset::Btc][&exp].legs[0].call_mid_usd,
            Some(4.9)
        );
    }

    #[test]
    fn assemble_isolates_expiries_within_an_asset() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let near = now + time::Duration::days(7);
        let far = now + time::Duration::days(40);
        let rows = vec![
            row("deribit", "btc", near, 100.0, "call", Some(1.0), Some(0.5)),
            row("deribit", "btc", far, 100.0, "call", Some(2.0), Some(0.55)),
        ];
        let out = assemble_chains(rows, now);
        let by_expiry = &out[&Venue::Deribit].assets[&Asset::Btc];
        assert_eq!(by_expiry.len(), 2);
        assert!((by_expiry[&near].time_to_expiry.0 - 7.0 / 365.0).abs() < 1e-12);
        assert!((by_expiry[&far].time_to_expiry.0 - 40.0 / 365.0).abs() < 1e-12);
    }

    #[test]
    fn assemble_filters_non_finite_iv_and_mid() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let exp = now + time::Duration::days(7);
        let rows = vec![
            row(
                "deribit",
                "btc",
                exp,
                100.0,
                "call",
                Some(f64::NAN),
                Some(f64::INFINITY),
            ),
            row("deribit", "btc", exp, 100.0, "put", Some(2.0), Some(0.5)),
        ];
        let out = assemble_chains(rows, now);
        let leg = &out[&Venue::Deribit].assets[&Asset::Btc][&exp].legs[0];
        assert_eq!(leg.call_mid_usd, None, "NaN mid filtered to None");
        assert_eq!(leg.call_iv, None, "Inf iv filtered to None");
        assert_eq!(leg.put_mid_usd, Some(2.0));
        assert_eq!(leg.put_iv, Some(0.5));
    }

    #[test]
    fn assemble_uses_passed_now_for_time_to_expiry() {
        let now = datetime!(2026-01-01 00:00:00 UTC);
        let exp = datetime!(2026-01-31 00:00:00 UTC);
        let rows = vec![row(
            "deribit",
            "btc",
            exp,
            100.0,
            "call",
            Some(1.0),
            Some(0.5),
        )];
        let out = assemble_chains(rows, now);
        let tt = out[&Venue::Deribit].assets[&Asset::Btc][&exp]
            .time_to_expiry
            .0;
        assert!((tt - 30.0 / 365.0).abs() < 1e-12);
    }

    #[test]
    fn assemble_empty_input_yields_empty_map() {
        let now = datetime!(2026-05-25 00:00:00 UTC);
        let out = assemble_chains(Vec::new(), now);
        assert!(out.is_empty());
    }
}
