//! Confidence score for the published index value (issue #62,
//! `METHODOLOGY.md` §5.1).
//!
//! Confidence is a single scalar in `[0.0, 1.0]` reflecting three
//! independent quality signals, multiplied:
//!
//! ```text
//! venue_term  = venues_live / venues_expected
//! fresh_term  = clamp(1 - max_quote_age_s / FRESH_BUDGET_S, 0, 1)
//! strike_term = clamp(strip_strikes / methodology_min_strikes, 0, 1)
//! confidence  = venue_term * fresh_term * strike_term
//! ```
//!
//! Multiplied (not averaged): each term is a necessary condition, so
//! any one term collapsing to zero must collapse the whole score.
//! A single venue with perfectly fresh data and a thin strip should
//! not publish `confidence = 1.0`.
//!
//! Saturating math throughout — every division guards against `0`
//! denominators, every output clamps to `[0, 1]`, and NaN is filtered
//! to `0.0`. The function is total: any inputs in, a finite value in
//! `[0, 1]` out.
//!
//! This is a *leading* indicator of degraded mode (which venues are
//! live, how fresh the feed is). It is **not** a forecast of index
//! error — that requires a backtest against the per-tick
//! confidence-vs-error correlation (research, not M2).

use std::time::Duration;

/// Freshness budget for the per-venue `max_quote_age` term. A venue
/// with `max_quote_age` ≥ this value contributes `fresh_term = 0`.
/// 60 s is twice the engine's `SNAPSHOT_FRESHNESS_SECS` (30 s) chain
/// query window, so any data the engine actually consumed has
/// `fresh_term ≥ 0.5`. The 60 s figure is documented in
/// `METHODOLOGY.md` §5.1.
pub const FRESH_BUDGET_S: f64 = 60.0;

/// Default per-venue strike-count target for `strike_term = 1.0`.
/// `MIN_STRIP_QUOTES = 5` is the absolute floor the strip builder
/// rejects below; 8 is the methodology-recommended count for a
/// defensible spline fit per `METHODOLOGY.md` §5.1.
pub const METHODOLOGY_MIN_STRIKES: usize = 8;

/// Inputs to [`score`]. Construct from per-venue snapshot data
/// (count of live venues, max staleness, minimum listed-strike count
/// across all per-venue near + next strips).
#[derive(Debug, Clone, Copy)]
pub struct ConfidenceInputs {
    /// Number of venues that produced a per-venue snapshot for this
    /// tick (i.e. survived the per-venue pipeline). Stale venues
    /// (per `max_quote_age > FRESH_BUDGET_S`) are NOT subtracted
    /// here — the freshness penalty is folded into `fresh_term`
    /// instead. Counting and freshness are kept orthogonal so each
    /// signal stays interpretable in isolation.
    pub venues_live: usize,
    /// Number of venues the engine expects to be live in steady
    /// state. Defaults to 3 (Deribit + OKX + Bybit) but is
    /// configurable via `ENGINE_VENUES_EXPECTED` so an operator can
    /// dial it down during the period when only a subset of
    /// connectors has shipped — otherwise confidence prematurely
    /// caps at `live / 3` instead of `live / live_target`.
    pub venues_expected: usize,
    /// Maximum quote age across every live venue's newest tick. Per
    /// the formula a venue with `max_quote_age > FRESH_BUDGET_S`
    /// drives `fresh_term` to zero, which (after multiplication)
    /// drives the whole score to zero — degraded data must not
    /// publish as "high confidence".
    pub max_quote_age: Duration,
    /// Minimum listed-strike count across all per-venue × near/next
    /// strips. Min (not mean) because a single thin strip is a
    /// quality regression even if the other strips are fat —
    /// confidence is the worst case the user is exposed to.
    pub strip_strikes: usize,
    /// Listed-strike count at which `strike_term = 1.0`. The
    /// methodology-recommended value is
    /// [`METHODOLOGY_MIN_STRIKES`]; tests use lower values to keep
    /// fixtures small.
    pub methodology_min_strikes: usize,
}

/// Compute the confidence score. Returns a value in `[0.0, 1.0]`.
///
/// Special cases:
/// - `venues_expected == 0` or `methodology_min_strikes == 0` →
///   the corresponding term is clamped to `1.0` (skipped from the
///   product) rather than panicking on the division. Operators
///   should never set these to zero, but a defensive math path
///   keeps the engine from refusing to publish on a misconfigured
///   env var.
/// - NaN / non-finite intermediate → `0.0`. The score is
///   conservative by construction.
#[must_use]
pub fn score(inputs: ConfidenceInputs) -> f64 {
    #[allow(clippy::cast_precision_loss)] // counts are small (≤ a few thousand)
    let venue_term = if inputs.venues_expected == 0 {
        1.0
    } else {
        (inputs.venues_live as f64 / inputs.venues_expected as f64).clamp(0.0, 1.0)
    };

    let fresh_term = {
        let age_s = inputs.max_quote_age.as_secs_f64();
        (1.0 - age_s / FRESH_BUDGET_S).clamp(0.0, 1.0)
    };

    #[allow(clippy::cast_precision_loss)] // small counts
    let strike_term = if inputs.methodology_min_strikes == 0 {
        1.0
    } else {
        (inputs.strip_strikes as f64 / inputs.methodology_min_strikes as f64).clamp(0.0, 1.0)
    };

    let raw = venue_term * fresh_term * strike_term;
    if raw.is_finite() {
        raw.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn full_inputs() -> ConfidenceInputs {
        ConfidenceInputs {
            venues_live: 3,
            venues_expected: 3,
            max_quote_age: Duration::from_secs(0),
            strip_strikes: METHODOLOGY_MIN_STRIKES,
            methodology_min_strikes: METHODOLOGY_MIN_STRIKES,
        }
    }

    #[test]
    fn all_full_signals_yields_one() {
        // 3/3 venues, zero quote age, full strike count → 1.0.
        assert_eq!(score(full_inputs()), 1.0);
    }

    #[test]
    fn two_of_three_venues_yields_two_thirds() {
        // 2/3 venues, rest perfect → 0.6667 ± epsilon.
        let mut i = full_inputs();
        i.venues_live = 2;
        let s = score(i);
        assert!((s - 2.0 / 3.0).abs() < 1e-12, "expected ≈ 0.667, got {s}");
    }

    #[test]
    fn single_venue_quarter_strikes_yields_quarter() {
        // 1/3 venues × 1.0 fresh × (2/8 strikes) = 1/3 * 1/4 = 1/12.
        let mut i = full_inputs();
        i.venues_live = 1;
        i.strip_strikes = 2;
        let s = score(i);
        assert!((s - 1.0 / 12.0).abs() < 1e-12, "expected ≈ 0.083, got {s}");
    }

    #[test]
    fn quote_age_beyond_budget_collapses_to_zero() {
        // 90 s age > 60 s FRESH_BUDGET_S → fresh_term clamps to 0,
        // so the whole score is 0 regardless of venues + strikes.
        let mut i = full_inputs();
        i.max_quote_age = Duration::from_secs(90);
        assert_eq!(score(i), 0.0);
    }

    #[test]
    fn quote_age_half_budget_yields_half_fresh() {
        // 30 s age / 60 s budget → fresh_term = 0.5. Other terms
        // = 1.0, so confidence = 0.5.
        let mut i = full_inputs();
        i.max_quote_age = Duration::from_secs(30);
        let s = score(i);
        assert!((s - 0.5).abs() < 1e-12, "got {s}");
    }

    #[test]
    fn output_clamped_above_one_when_overprovisioned() {
        // 4 venues live, 3 expected → venue_term = 4/3 but
        // clamp guarantees ≤ 1.0. Defensive against a future
        // misconfigured env that under-estimates venue count.
        let mut i = full_inputs();
        i.venues_live = 4;
        assert_eq!(score(i), 1.0);
    }

    #[test]
    fn output_clamped_above_one_when_strikes_exceed_target() {
        // 16 strikes vs 8 target → strike_term clamps to 1.0.
        let mut i = full_inputs();
        i.strip_strikes = METHODOLOGY_MIN_STRIKES * 2;
        assert_eq!(score(i), 1.0);
    }

    #[test]
    fn zero_venues_expected_skips_venue_term() {
        // Defensive path: ENGINE_VENUES_EXPECTED=0 should not
        // panic on divide-by-zero. The term clamps to 1.0,
        // collapsing the score onto fresh × strike.
        let mut i = full_inputs();
        i.venues_expected = 0;
        // Result is fresh * strike = 1 * 1 = 1.
        assert_eq!(score(i), 1.0);
    }

    #[test]
    fn zero_methodology_min_strikes_skips_strike_term() {
        let mut i = full_inputs();
        i.methodology_min_strikes = 0;
        assert_eq!(score(i), 1.0);
    }

    #[test]
    fn zero_venues_live_yields_zero() {
        let mut i = full_inputs();
        i.venues_live = 0;
        assert_eq!(score(i), 0.0);
    }

    #[test]
    fn zero_strikes_yields_zero() {
        let mut i = full_inputs();
        i.strip_strikes = 0;
        assert_eq!(score(i), 0.0);
    }

    #[test]
    fn output_always_finite() {
        // Belt-and-braces: random plausible inputs always produce
        // a finite value in [0, 1].
        for live in 0..=5 {
            for age in [0, 10, 30, 59, 60, 90, 600] {
                for strikes in 0..=20 {
                    let s = score(ConfidenceInputs {
                        venues_live: live,
                        venues_expected: 3,
                        max_quote_age: Duration::from_secs(age),
                        strip_strikes: strikes,
                        methodology_min_strikes: METHODOLOGY_MIN_STRIKES,
                    });
                    assert!(
                        s.is_finite(),
                        "non-finite at live={live} age={age} strikes={strikes}: {s}"
                    );
                    assert!((0.0..=1.0).contains(&s), "out of range: {s}");
                }
            }
        }
    }
}
