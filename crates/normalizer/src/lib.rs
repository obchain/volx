//! Per-tick quote filters between ingestion and the engine (issue #12).
//!
//! Implements the **normalizer-layer** filters from METHODOLOGY.md §3.1:
//! staleness, crossed/locked book, wide spread, below intrinsic. The
//! **engine-layer** filters (missing IV, missing side, strip min size,
//! §3.2) live in the engine crate because they need a full snapshot.
//!
//! Each drop increments `volx_normalizer_filtered_total{reason}` via the
//! `metrics` facade; the Prometheus exporter is wired in #11 without
//! changing the emit sites here.
//!
//! ## Scope deliberately out of #12
//!
//! - **Per-side dropping.** The methodology says "a side that fails any
//!   rule is treated as missing for the next downstream snapshot"; this
//!   implementation drops the whole tick. The engine reads the latest tick
//!   per `(strike, expiry)`, so a dropped tick = engine sees the previous
//!   one. Per-side semantics is a follow-up once the strip builder lands
//!   and shows whether the simplification costs accuracy.
//! - **Zero-bid wing-termination.** Listed in the original issue #12 spec
//!   but explicitly dropped by METHODOLOGY.md §3 ("not part of the
//!   canonical engine pipeline; replaced by fitted-IV smoothing in §4.3").
//! - **Dedup on `(venue, instrument, ts)`** → issue #13.
//! - **Persist to `ClickHouse` + Redis pubsub** → issue #16.

pub mod config;
pub mod dedup;
pub mod sink;

pub use config::NormalizerConfig;
pub use dedup::{DedupKey, DedupOutcome, Deduper};
pub use sink::{
    ClickHouseBatcher, ClickHouseSinkConfig, RedisPublisher, RedisSinkConfig, run_default_pipeline,
    run_pipeline,
};

use time::OffsetDateTime;
use tracing::trace;
use volx_shared_types::{OptionKind, OptionTick};

/// Why a tick was dropped. The string form is the `reason` Prometheus label
/// — keep it stable across releases (consumer dashboards key on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterReason {
    /// Tick `received_at` is older than `max_age_secs`.
    Stale,
    /// Book is crossed or locked (`ask ≤ bid`).
    Crossed,
    /// Spread (`(ask − bid) / mid`) exceeds `max_spread_ratio`.
    WideSpread,
    /// Mid is below the option's intrinsic value (no-arbitrage violation).
    BelowIntrinsic,
}

impl FilterReason {
    /// Stable label string for the `volx_normalizer_filtered_total` counter.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::Crossed => "crossed",
            Self::WideSpread => "wide_spread",
            Self::BelowIntrinsic => "below_intrinsic",
        }
    }
}

/// Outcome of running the filter pipeline on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOutcome {
    /// Tick passes every filter; forward downstream.
    Pass,
    /// Tick was rejected — drop it. `reason` is also recorded in the
    /// `volx_normalizer_filtered_total` counter.
    Drop(FilterReason),
}

/// Per-tick filter pipeline.
///
/// Stateless besides the [`NormalizerConfig`]; safe to share across tasks
/// via `&Normalizer`. The caller supplies `now` so the filter is
/// deterministic in tests (no internal clock reads).
#[derive(Debug, Clone)]
pub struct Normalizer {
    config: NormalizerConfig,
}

impl Normalizer {
    /// Build a normalizer with the supplied thresholds.
    #[must_use]
    pub fn new(config: NormalizerConfig) -> Self {
        Self { config }
    }

    /// Build with methodology defaults.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(NormalizerConfig::default())
    }

    /// Threshold view for tests / diagnostics.
    #[must_use]
    pub fn config(&self) -> &NormalizerConfig {
        &self.config
    }

    /// Run every filter on `tick` (in order: staleness → crossed → spread →
    /// intrinsic) and return the first failure if any. Filters that need
    /// fields the tick is missing (e.g. spread needs bid + ask + mid) are
    /// skipped — the matching engine-layer filter catches the missing side.
    ///
    /// `now` is the wall-clock used by the staleness check. Pass
    /// `OffsetDateTime::now_utc()` in production; test code passes a fixed
    /// time so behaviour is reproducible.
    #[must_use]
    pub fn check_tick(&self, tick: &OptionTick, now: OffsetDateTime) -> FilterOutcome {
        // 1. Staleness. `time::Duration` is signed; a tick whose
        //    `received_at` is in the future (venue clock skew, replayed
        //    fixture, etc.) yields a negative age and would silently pass
        //    a naive `> max_age` test. Treat future-dated ticks as stale
        //    too — they are just as suspect as old ones.
        let age_secs = (now - tick.received_at).as_seconds_f64();
        if !(0.0..=self.config.max_age_secs).contains(&age_secs) {
            return Self::record_drop(FilterReason::Stale);
        }

        // 2. Crossed / locked. Needs both sides; if either is missing the
        //    quote is incomplete and a different filter (or engine layer)
        //    handles it. A negative bid or ask means the book itself is
        //    malformed (some venues briefly publish negatives during a
        //    disconnect-replay race); roll that into `Crossed` rather than
        //    inventing a new label that would mislabel the symptom — the
        //    quote is "not a valid two-sided market," same root cause.
        if let (Some(bid), Some(ask)) = (tick.bid, tick.ask)
            && (ask <= bid || bid < 0.0 || ask < 0.0)
        {
            return Self::record_drop(FilterReason::Crossed);
        }

        // 3. Wide spread. Needs both sides + a positive mid.
        if let (Some(bid), Some(ask), Some(mid)) = (tick.bid, tick.ask, tick.mid)
            && mid > 0.0
            && (ask - bid) / mid > self.config.max_spread_ratio
        {
            return Self::record_drop(FilterReason::WideSpread);
        }

        // 4. Below intrinsic. Needs mid + strike + underlying + kind.
        if let Some(mid) = tick.mid {
            let intrinsic = match tick.kind {
                OptionKind::Call => (tick.underlying - tick.strike).max(0.0),
                OptionKind::Put => (tick.strike - tick.underlying).max(0.0),
            };
            if mid + self.config.intrinsic_tolerance < intrinsic {
                return Self::record_drop(FilterReason::BelowIntrinsic);
            }
        }

        FilterOutcome::Pass
    }

    /// Increment the drop counter + emit a trace event, returning the
    /// `FilterOutcome::Drop(reason)`. Associated (not `&self`) because the
    /// drop side does not depend on any normalizer state — only on the
    /// reason — and clippy `unused_self` is correct to flag it otherwise.
    fn record_drop(reason: FilterReason) -> FilterOutcome {
        // Counter name from issue #12 acceptance. Label is keyed on the
        // stable `FilterReason::as_label()` string — dashboard authors can
        // rely on the set { stale, crossed, wide_spread, below_intrinsic }.
        metrics::counter!("volx_normalizer_filtered_total", "reason" => reason.as_label())
            .increment(1);
        trace!(reason = reason.as_label(), "tick dropped");
        FilterOutcome::Drop(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;
    use volx_shared_types::{Asset, Venue};

    fn fresh_tick(now: OffsetDateTime) -> OptionTick {
        OptionTick {
            venue: Venue::Deribit,
            asset: Asset::Btc,
            expiry: datetime!(2026-06-26 08:00:00 UTC),
            strike: 70_000.0,
            kind: OptionKind::Call,
            bid: Some(2_500.0),
            ask: Some(2_550.0),
            mid: Some(2_525.0),
            iv: Some(0.65),
            underlying: 68_500.0,
            open_interest: 100.0,
            volume_24h: 10.0,
            received_at: now,
        }
    }

    fn now() -> OffsetDateTime {
        datetime!(2026-05-25 12:00:00 UTC)
    }

    #[test]
    fn fresh_quote_passes_every_filter() {
        let n = Normalizer::with_defaults();
        assert_eq!(n.check_tick(&fresh_tick(now()), now()), FilterOutcome::Pass);
    }

    #[test]
    fn stale_quote_dropped() {
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.received_at = now() - time::Duration::seconds(6);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Stale)
        );
    }

    #[test]
    fn quote_at_max_age_still_passes() {
        // METHODOLOGY says drop if `> 5s old`, so exactly 5s is still valid.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.received_at = now() - time::Duration::seconds(5);
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }

    #[test]
    fn crossed_quote_dropped() {
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(2_600.0); // > ask
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Crossed)
        );
    }

    #[test]
    fn locked_quote_dropped() {
        // ask == bid is "locked"; still drop.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(2_550.0);
        tick.ask = Some(2_550.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Crossed)
        );
    }

    #[test]
    fn wide_spread_dropped() {
        // bid 1000, ask 2000, mid 1500: spread/mid = 1000/1500 = 0.667 > 0.30
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(1_000.0);
        tick.ask = Some(2_000.0);
        tick.mid = Some(1_500.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::WideSpread)
        );
    }

    #[test]
    fn narrow_spread_passes() {
        // bid 2480, ask 2520, mid 2500: spread/mid = 40/2500 = 0.016
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(2_480.0);
        tick.ask = Some(2_520.0);
        tick.mid = Some(2_500.0);
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }

    #[test]
    fn missing_side_skips_two_sided_filters() {
        // Only one side present — crossed + spread can't run. The remaining
        // filters (staleness + intrinsic) should still apply.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = None;
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }

    #[test]
    fn call_below_intrinsic_dropped() {
        // ITM call: strike 50k, underlying 70k → intrinsic 20k.
        // mid 100 USD is way below. Drop.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.strike = 50_000.0;
        tick.underlying = 70_000.0;
        tick.mid = Some(100.0);
        tick.bid = Some(99.0);
        tick.ask = Some(101.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::BelowIntrinsic)
        );
    }

    #[test]
    fn put_below_intrinsic_dropped() {
        // ITM put: strike 90k, underlying 70k → intrinsic 20k.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.kind = OptionKind::Put;
        tick.strike = 90_000.0;
        tick.underlying = 70_000.0;
        tick.mid = Some(100.0);
        tick.bid = Some(99.0);
        tick.ask = Some(101.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::BelowIntrinsic)
        );
    }

    #[test]
    fn mid_exactly_at_intrinsic_passes() {
        // Boundary: mid == intrinsic. The `+ tolerance` allows it.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.strike = 50_000.0;
        tick.underlying = 70_000.0;
        tick.mid = Some(20_000.0);
        tick.bid = Some(19_999.0);
        tick.ask = Some(20_001.0);
        // Spread: 2/20000 = 0.0001 — well inside.
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }

    #[test]
    fn out_of_the_money_intrinsic_zero_passes() {
        // OTM call: strike 100k, underlying 70k → intrinsic 0. Any mid >= 0 ok.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.strike = 100_000.0;
        tick.underlying = 70_000.0;
        tick.mid = Some(50.0);
        tick.bid = Some(45.0);
        tick.ask = Some(55.0);
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }

    #[test]
    fn filter_priority_staleness_first() {
        // A stale quote that would also fail crossed/spread/intrinsic must
        // report `Stale` — the first failure wins. Useful for telemetry
        // because operators want to see the root cause, not a downstream
        // symptom.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.received_at = now() - time::Duration::seconds(10);
        tick.bid = Some(2_600.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Stale)
        );
    }

    #[test]
    fn filter_reason_labels_are_stable() {
        // Lock the wire format for dashboards.
        assert_eq!(FilterReason::Stale.as_label(), "stale");
        assert_eq!(FilterReason::Crossed.as_label(), "crossed");
        assert_eq!(FilterReason::WideSpread.as_label(), "wide_spread");
        assert_eq!(FilterReason::BelowIntrinsic.as_label(), "below_intrinsic");
    }

    #[test]
    fn future_dated_quote_dropped_as_stale() {
        // Negative age (clock skew). A naive `> max_age` test would let
        // this slip through; we reject it as `Stale` so a corrupt
        // received_at can't pose as fresh data.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.received_at = now() + time::Duration::seconds(2);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Stale)
        );
    }

    #[test]
    fn negative_bid_dropped_as_crossed() {
        // Malformed book (venue glitch) — roll into the Crossed label.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(-1.0);
        tick.ask = Some(2_550.0);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Crossed)
        );
    }

    #[test]
    fn negative_ask_dropped_as_crossed() {
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(0.0);
        tick.ask = Some(-0.5);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::Crossed)
        );
    }

    #[test]
    fn zero_bid_not_crossed() {
        // Empty bid side (`bid == 0`) is not "malformed"; it's common on
        // illiquid deep OTM options. The book is not crossed — the spread
        // filter (separately) will drop the tick because spread/mid = 2
        // always when bid is 0. We only assert the failure isn't mislabeled
        // as `Crossed`.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.bid = Some(0.0);
        tick.ask = Some(0.0001);
        tick.mid = Some(0.00005);
        assert_eq!(
            n.check_tick(&tick, now()),
            FilterOutcome::Drop(FilterReason::WideSpread)
        );
    }

    #[test]
    fn intrinsic_tolerance_absorbs_venue_rounding() {
        // Deep ITM call: strike 50k, underlying 70k → intrinsic 20k.
        // Mid is one-tenth of a cent below intrinsic — well within
        // typical venue coin→USD conversion rounding. Must pass.
        let n = Normalizer::with_defaults();
        let mut tick = fresh_tick(now());
        tick.strike = 50_000.0;
        tick.underlying = 70_000.0;
        tick.mid = Some(19_999.999);
        tick.bid = Some(19_999.0);
        tick.ask = Some(20_001.0);
        assert_eq!(n.check_tick(&tick, now()), FilterOutcome::Pass);
    }
}
