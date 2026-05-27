//! Per-expiry strip builder (issue #17, `METHODOLOGY.md` §4.2 + §4.3 + §4.5).
//!
//! Input: an [`ExpiryChain`] — calls + puts grouped per strike, plus the
//! year-fraction `time_to_expiry`. The downstream snapshot reader
//! (issue #20) is responsible for the venue → chain transformation,
//! including the `time_to_expiry` computation. This module is pure
//! numerics; no I/O, no clocks.
//!
//! Output: a [`Strip`] of `(K, Q(K), iv(K))` triples on the 801-point
//! dense grid, plus the forward `F` and the K₀ split point.
//!
//! Stages (METHODOLOGY references in parens):
//!
//! 1. **Forward** (§4.2) — argmin |C − P| over strikes with both legs
//!    quoted above `1e-9` USD; `F = K* + e^{rT}(C − P)`. Tie-break:
//!    smallest strike.
//! 2. **IV surface** (§4.3) — collect `mark_iv` per listed strike (call
//!    primary, put fallback), fit a natural cubic spline in log-moneyness
//!    `x = ln(K / F)`, sample on the 801-point linear K grid between
//!    `K_min` and `K_max`, clamp to `[1e-4, 5.0]`.
//! 3. **Carr-Madan OTM prices** (§4.5) — Black-Scholes from the fitted
//!    IV; `Q(K)` = put for `K < K₀`, call for `K > K₀`, average at `K₀`.
//!    `K₀` = largest dense-grid point at or below `F`.
//!
//! Methodology pin: the grid size `801`, the natural-spline boundary
//! condition, the no-extrapolation rule, and the IV clamp bounds are
//! constants in [`volx_shared_types::strip`] (or const'd locally) so a
//! reader can trace the wire-format → engine path without grep guessing.

use volx_shared_types::strip::{MAX_FITTED_IV, MIN_FITTED_IV, MIN_STRIP_QUOTES, Strip, StripQuote};
use volx_shared_types::units::Years;

use crate::bs::{call_price, put_price};
use crate::spline::{NaturalCubicSpline, SplineError};

/// Number of strikes on the dense Carr-Madan grid (§4.3 step 4 — pinned).
pub const DENSE_GRID_POINTS: usize = 801;

/// Minimum non-zero leg price for the forward picker (§4.2).
const MIN_QUOTED_USD: f64 = 1e-9;

/// One listed strike with optional call + put quotes.
///
/// `*_mid_usd` are the USD-denominated mid prices from the normalizer
/// (`OptionTick.mid` already in USD per `METHODOLOGY.md` §2.1). `*_iv` is
/// the venue-published `mark_iv` (decimal fraction, not percent).
///
/// Per-side `None`s are expected — Deribit publishes both legs but the
/// normalizer's filters (#12) may invalidate one side independently.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChainLeg {
    pub strike: f64,
    pub call_mid_usd: Option<f64>,
    pub put_mid_usd: Option<f64>,
    pub call_iv: Option<f64>,
    pub put_iv: Option<f64>,
}

/// All listed strikes for a single expiry plus the year-fraction
/// time-to-expiry. Legs do **not** need to be pre-sorted; the builder
/// sorts by strike before any numerical step.
#[derive(Debug, Clone)]
pub struct ExpiryChain {
    pub time_to_expiry: Years,
    pub legs: Vec<ChainLeg>,
}

/// Why a strip-build attempt failed. Returned errors are non-fatal at
/// the engine level — the scheduler (issue #20) records the reason on
/// the published index status and continues with the next snapshot.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("expiry has no strike with both call and put quoted > {MIN_QUOTED_USD} USD")]
    NoTwoSidedStrike,
    #[error("expiry T = {0} is not finite or not positive")]
    NonPositiveT(f64),
    #[error(
        "only {0} strike(s) carry a usable IV after filtering; need ≥ {MIN_STRIP_QUOTES} for a defensible spline"
    )]
    TooFewIv(usize),
    #[error("forward {forward} is outside listed strike range [{k_min}, {k_max}]")]
    ForwardOutsideStrikeRange {
        forward: f64,
        k_min: f64,
        k_max: f64,
    },
    #[error("spline fit failed: {0}")]
    SplineFit(#[from] SplineError),
    #[error("spline sample is NaN at K={0}")]
    SplineDomainError(f64),
    #[error(
        "dense grid produced {0} valid quotes, below MIN_STRIP_QUOTES={MIN_STRIP_QUOTES}; should never happen with a 801-point grid (bug)"
    )]
    DenseGridUnderfilled(usize),
}

/// Build a [`Strip`] from a per-expiry chain. Uses `r = 0` per §4.4.
pub fn build_strip(chain: &ExpiryChain) -> Result<Strip, BuildError> {
    build_strip_with_rate(chain, 0.0)
}

/// Variant that takes an explicit `r` — only useful for tests and for
/// the future-version §4.4 bump to a USDC/USDT lending rate.
///
/// # Panics
///
/// Sort uses `partial_cmp` and will panic if the chain contains a
/// strike that is `NaN`. The engine deliberately does not pre-validate
/// strikes (the normalizer + chain assembler upstream should never
/// emit a NaN strike; the panic surfaces the bug rather than silently
/// dropping the leg). Callers that ingest from an untrusted source
/// should filter `!leg.strike.is_finite()` before calling.
pub fn build_strip_with_rate(chain: &ExpiryChain, r: f64) -> Result<Strip, BuildError> {
    let t = chain.time_to_expiry.0;
    if !t.is_finite() || t <= 0.0 {
        return Err(BuildError::NonPositiveT(t));
    }

    // Defensive copy + sort by strike. The downstream chain assembler
    // should already provide sorted input, but enforcing it here means
    // the picker + spline don't have to defend separately.
    let mut legs = chain.legs.clone();
    legs.sort_by(|a, b| {
        a.strike
            .partial_cmp(&b.strike)
            .expect("strike NaN should be filtered earlier")
    });

    // 1. Forward via put-call parity.
    let forward = pick_forward(&legs, t, r)?;

    // 2. Listed-strike IVs in log-moneyness coordinates.
    let mut xs = Vec::with_capacity(legs.len());
    let mut ys = Vec::with_capacity(legs.len());
    let mut k_min = f64::INFINITY;
    let mut k_max = f64::NEG_INFINITY;
    for leg in &legs {
        let Some(iv) = pick_iv(leg) else { continue };
        if !leg.strike.is_finite() || leg.strike <= 0.0 {
            continue;
        }
        let x = (leg.strike / forward).ln();
        if !x.is_finite() || !iv.is_finite() {
            continue;
        }
        xs.push(x);
        ys.push(iv);
        if leg.strike < k_min {
            k_min = leg.strike;
        }
        if leg.strike > k_max {
            k_max = leg.strike;
        }
    }
    if xs.len() < MIN_STRIP_QUOTES {
        return Err(BuildError::TooFewIv(xs.len()));
    }
    // §4.3 step 3 disallows extrapolation; if F is outside the listed
    // strike range the dense grid will still cover [K_min, K_max] but
    // the spline is being asked to interpolate IVs that don't bracket
    // F — bail rather than publish a value built on extrapolation.
    if forward < k_min || forward > k_max {
        return Err(BuildError::ForwardOutsideStrikeRange {
            forward,
            k_min,
            k_max,
        });
    }

    // After sort-by-strike, `xs` is already strictly increasing because
    // `ln` is monotonic and strikes are unique by construction (one leg
    // per strike); the spline `fit` would reject otherwise.
    let spline = NaturalCubicSpline::fit(&xs, &ys)?;

    // 3. Dense grid: 801 points linearly between K_min and K_max.
    let mut quotes = Vec::with_capacity(DENSE_GRID_POINTS);
    #[allow(clippy::cast_precision_loss)]
    let step = (k_max - k_min) / (DENSE_GRID_POINTS as f64 - 1.0);
    // K₀ = largest dense point ≤ F. Computed inline so we can flag the
    // split-point quote for the average pass below.
    let mut k_zero_index: usize = 0;
    for j in 0..DENSE_GRID_POINTS {
        let k = if j + 1 == DENSE_GRID_POINTS {
            // Pin the right endpoint exactly to k_max — avoid a
            // float-roundoff `k > k_max` that would trip the spline's
            // no-extrapolation guard.
            k_max
        } else {
            #[allow(clippy::cast_precision_loss)]
            let j_f = j as f64;
            k_min + step * j_f
        };
        if k <= forward {
            k_zero_index = j;
        }

        let x = (k / forward).ln();
        let iv_raw = spline.eval(x).ok_or(BuildError::SplineDomainError(k))?;
        if !iv_raw.is_finite() {
            return Err(BuildError::SplineDomainError(k));
        }
        let iv = iv_raw.clamp(MIN_FITTED_IV, MAX_FITTED_IV);

        // Carr-Madan OTM rule. `put_price` / `call_price` already
        // discount internally, so no extra `e^{-rT}` factor here.
        let q_usd = if k < forward {
            put_price(forward, k, t, r, iv)
        } else {
            call_price(forward, k, t, r, iv)
        };

        quotes.push(StripQuote {
            strike: k,
            q_usd,
            iv,
        });
    }

    // K₀ average (§4.5): at the split index, replace `Q(K₀)` with
    // `(P_dense(K₀) + C_dense(K₀)) / 2`. The single price stored above
    // is the put leg (because `k_zero <= forward` → fell into the
    // `k < forward` branch above; or at `k == forward` we landed in
    // the `else` branch by the `<` test — handle both).
    {
        let split = &mut quotes[k_zero_index];
        let p = put_price(forward, split.strike, t, r, split.iv);
        let c = call_price(forward, split.strike, t, r, split.iv);
        split.q_usd = 0.5 * (p + c);
    }

    if quotes.len() < MIN_STRIP_QUOTES {
        return Err(BuildError::DenseGridUnderfilled(quotes.len()));
    }

    let k_zero = quotes[k_zero_index].strike;
    Ok(Strip {
        forward,
        k_zero,
        time_to_expiry: chain.time_to_expiry,
        quotes,
    })
}

/// Pick the listed strike `K*` whose `|C − P|` is smallest (§4.2),
/// restricted to strikes with both legs quoted above [`MIN_QUOTED_USD`].
/// `F = K* + e^{rT} · (C − P)`. Tie-break: smallest strike.
fn pick_forward(legs: &[ChainLeg], t: f64, r: f64) -> Result<f64, BuildError> {
    let growth = (r * t).exp();
    let mut best_diff = f64::INFINITY;
    let mut best_forward: Option<f64> = None;
    for leg in legs {
        let (Some(c), Some(p)) = (leg.call_mid_usd, leg.put_mid_usd) else {
            continue;
        };
        if !c.is_finite() || !p.is_finite() {
            continue;
        }
        if c <= MIN_QUOTED_USD || p <= MIN_QUOTED_USD {
            continue;
        }
        if !leg.strike.is_finite() || leg.strike <= 0.0 {
            continue;
        }
        let diff = (c - p).abs();
        // Strict `<` keeps the smallest-strike tie-break — `legs` is
        // already sorted ascending, so the first qualifying minimum
        // wins; a later equal diff does not displace it.
        if diff < best_diff {
            best_diff = diff;
            best_forward = Some(leg.strike + growth * (c - p));
        }
    }
    let f = best_forward.ok_or(BuildError::NoTwoSidedStrike)?;
    if !f.is_finite() || f <= 0.0 {
        return Err(BuildError::ForwardOutsideStrikeRange {
            forward: f,
            k_min: f64::NAN,
            k_max: f64::NAN,
        });
    }
    Ok(f)
}

/// Methodology §4.3 step 1: prefer call-side IV; fall back to put-side
/// if the call leg is missing **or non-finite**. Deribit publishes a
/// single IV per `(strike, expiry)` so the two are equal in practice,
/// but the normalizer can also leave the call-side field populated with
/// `NaN` when the leg failed the filter pipeline mid-quote — treat
/// that the same as "call missing" and try the put.
///
/// (`Option::or` short-circuits on `Some(_)` regardless of contents, so
/// a naive `call.or(put).filter(finite)` drops the strike entirely when
/// `call = Some(NaN)`. Filter each side **before** combining.)
fn pick_iv(leg: &ChainLeg) -> Option<f64> {
    leg.call_iv
        .filter(|v| v.is_finite())
        .or_else(|| leg.put_iv.filter(|v| v.is_finite()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use volx_shared_types::strip::{MAX_FITTED_IV, MIN_FITTED_IV};

    /// Build a synthetic flat-IV chain centered on a known forward.
    ///
    /// Constructs `n_pairs` symmetric strikes around `forward` at step
    /// `step`, prices each leg via BS with `iv`, then returns the
    /// chain. The forward picker should recover `forward` exactly.
    fn flat_iv_chain(forward: f64, step: f64, n_pairs: usize, t: f64, iv: f64) -> ExpiryChain {
        let mut legs = Vec::new();
        #[allow(clippy::cast_possible_wrap)]
        let half = n_pairs as i64;
        for i in -half..=half {
            #[allow(clippy::cast_precision_loss)]
            let k = forward + step * i as f64;
            if k <= 0.0 {
                continue;
            }
            let c = call_price(forward, k, t, 0.0, iv);
            let p = put_price(forward, k, t, 0.0, iv);
            legs.push(ChainLeg {
                strike: k,
                call_mid_usd: Some(c),
                put_mid_usd: Some(p),
                call_iv: Some(iv),
                put_iv: Some(iv),
            });
        }
        ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        }
    }

    #[test]
    fn flat_iv_recovers_forward_exactly() {
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        // F = K* + (C - P). At the ATM strike both legs have equal
        // prices → diff = 0 → picker selects K* = 100, forward = 100.
        assert!((strip.forward - 100.0).abs() < 1e-9, "F={}", strip.forward);
    }

    #[test]
    fn forward_picker_tiebreak_smallest_strike() {
        // Two strikes with identical |C − P| = 0 (synthetic): the
        // smaller strike must win (§4.2 tie-break).
        let legs = vec![
            ChainLeg {
                strike: 90.0,
                call_mid_usd: Some(10.0),
                put_mid_usd: Some(10.0),
                call_iv: Some(0.5),
                put_iv: Some(0.5),
            },
            ChainLeg {
                strike: 110.0,
                call_mid_usd: Some(5.0),
                put_mid_usd: Some(5.0),
                call_iv: Some(0.5),
                put_iv: Some(0.5),
            },
        ];
        // Need at least 5 legs for the spline; pad with IV-only legs.
        let mut chain = ExpiryChain {
            time_to_expiry: Years(0.25),
            legs,
        };
        for k in [80.0, 100.0, 120.0, 130.0] {
            chain.legs.push(ChainLeg {
                strike: k,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(0.5),
                put_iv: Some(0.5),
            });
        }
        let strip = build_strip(&chain).unwrap();
        // F = 90 + (10 − 10) = 90 (smaller-strike tie-break).
        assert!((strip.forward - 90.0).abs() < 1e-12);
    }

    #[test]
    fn k_zero_is_largest_dense_at_or_below_forward() {
        let chain = flat_iv_chain(100.0, 10.0, 5, 0.5, 0.6);
        let strip = build_strip(&chain).unwrap();
        assert!(strip.k_zero <= strip.forward);
        // The next grid point above k_zero must exceed forward (= it is
        // the *largest* point ≤ F). The dense grid is monotonic so it
        // suffices to check the immediate successor.
        let idx = strip
            .quotes
            .iter()
            .position(|q| (q.strike - strip.k_zero).abs() < 1e-12)
            .unwrap();
        if idx + 1 < strip.quotes.len() {
            assert!(strip.quotes[idx + 1].strike > strip.forward);
        }
    }

    #[test]
    fn dense_grid_has_801_points() {
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        assert_eq!(strip.quotes.len(), DENSE_GRID_POINTS);
    }

    #[test]
    fn dense_grid_strikes_are_monotonic_and_finite() {
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        for w in strip.quotes.windows(2) {
            assert!(w[1].strike > w[0].strike, "not monotonic at {w:?}");
            assert!(w[0].q_usd.is_finite());
            assert!(w[0].iv.is_finite());
        }
    }

    #[test]
    fn fitted_iv_is_clamped_in_range() {
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        for q in &strip.quotes {
            assert!(
                q.iv >= MIN_FITTED_IV && q.iv <= MAX_FITTED_IV,
                "iv={}",
                q.iv
            );
        }
    }

    #[test]
    fn flat_iv_recovers_iv_at_every_grid_point() {
        // With listed IV = 0.5 at every strike the natural-spline fit
        // is a constant, so every dense point should sample 0.5 (modulo
        // tiny tridiagonal numerical noise).
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        for q in &strip.quotes {
            assert!(
                (q.iv - 0.5).abs() < 1e-9,
                "iv at K={} drifted to {}",
                q.strike,
                q.iv
            );
        }
    }

    #[test]
    fn k_zero_quote_is_average_of_call_and_put() {
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        let split = strip
            .quotes
            .iter()
            .find(|q| (q.strike - strip.k_zero).abs() < 1e-12)
            .unwrap();
        let p = put_price(strip.forward, split.strike, 0.25, 0.0, split.iv);
        let c = call_price(strip.forward, split.strike, 0.25, 0.0, split.iv);
        assert!((split.q_usd - 0.5 * (p + c)).abs() < 1e-12);
    }

    #[test]
    fn rejects_chain_without_two_sided_strike() {
        let legs = (0..6)
            .map(|i| ChainLeg {
                strike: 90.0 + 10.0 * f64::from(i),
                call_mid_usd: Some(1.0),
                put_mid_usd: None,
                call_iv: Some(0.5),
                put_iv: None,
            })
            .collect();
        let chain = ExpiryChain {
            time_to_expiry: Years(0.25),
            legs,
        };
        assert!(matches!(
            build_strip(&chain),
            Err(BuildError::NoTwoSidedStrike)
        ));
    }

    #[test]
    fn rejects_chain_with_too_few_iv() {
        // Three legs only — below MIN_STRIP_QUOTES=5.
        let mut chain = flat_iv_chain(100.0, 5.0, 1, 0.25, 0.5); // 3 legs
        // Make the picker happy but spline starvation true: strip IVs.
        for leg in &mut chain.legs[2..] {
            leg.call_iv = None;
            leg.put_iv = None;
        }
        match build_strip(&chain) {
            Err(BuildError::TooFewIv(_) | BuildError::NoTwoSidedStrike) => {}
            other => panic!("expected TooFewIv or NoTwoSidedStrike, got {other:?}"),
        }
    }

    #[test]
    fn rejects_zero_or_negative_t() {
        let mut chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        chain.time_to_expiry = Years(0.0);
        assert!(matches!(
            build_strip(&chain),
            Err(BuildError::NonPositiveT(_))
        ));
        chain.time_to_expiry = Years(-1.0);
        assert!(matches!(
            build_strip(&chain),
            Err(BuildError::NonPositiveT(_))
        ));
    }

    #[test]
    fn pick_iv_falls_back_to_put_when_call_is_nan() {
        // Regression: `Option::or` short-circuits on `Some(NaN)`, so a
        // naive `call.or(put).filter(finite)` would silently drop the
        // strike even though the put-side IV is usable. The fix filters
        // each side independently.
        let leg = ChainLeg {
            strike: 100.0,
            call_mid_usd: Some(1.0),
            put_mid_usd: Some(1.0),
            call_iv: Some(f64::NAN),
            put_iv: Some(0.42),
        };
        assert_eq!(pick_iv(&leg), Some(0.42));
    }

    #[test]
    fn pick_iv_skips_both_when_neither_is_finite() {
        let leg = ChainLeg {
            strike: 100.0,
            call_mid_usd: Some(1.0),
            put_mid_usd: Some(1.0),
            call_iv: Some(f64::NAN),
            put_iv: Some(f64::INFINITY),
        };
        assert_eq!(pick_iv(&leg), None);
    }

    #[test]
    fn irregular_strike_spacing_still_builds() {
        // Asymmetric / non-uniform listed strikes around F=100. The
        // dense grid runs linearly between K_min and K_max regardless
        // of the input spacing — the spline is what handles the
        // irregularity. Build must succeed and the dense grid must
        // still be monotonic.
        let t = 0.25;
        let iv = 0.5;
        let strikes = [50.0, 70.0, 85.0, 95.0, 100.0, 105.0, 130.0, 200.0];
        let legs = strikes
            .iter()
            .map(|&k| ChainLeg {
                strike: k,
                call_mid_usd: Some(call_price(100.0, k, t, 0.0, iv)),
                put_mid_usd: Some(put_price(100.0, k, t, 0.0, iv)),
                call_iv: Some(iv),
                put_iv: Some(iv),
            })
            .collect();
        let chain = ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        };
        let strip = build_strip(&chain).unwrap();
        assert_eq!(strip.quotes.len(), DENSE_GRID_POINTS);
        for w in strip.quotes.windows(2) {
            assert!(w[1].strike > w[0].strike);
        }
        // Forward should still recover near 100 — picker tie-breaks on
        // smallest |C − P|, which is the K=100 ATM leg.
        assert!((strip.forward - 100.0).abs() < 1e-9, "F={}", strip.forward);
    }

    #[test]
    fn very_wide_strike_range_builds_with_dense_grid_spanning_full_range() {
        // K spans an order of magnitude (10 → 1000) around F=100. Strip
        // builder should still produce a 801-point grid covering the
        // whole listed range.
        let t = 0.25;
        let iv = 0.5;
        let strikes = [10.0, 30.0, 80.0, 100.0, 120.0, 300.0, 1000.0];
        let legs = strikes
            .iter()
            .map(|&k| ChainLeg {
                strike: k,
                call_mid_usd: Some(call_price(100.0, k, t, 0.0, iv)),
                put_mid_usd: Some(put_price(100.0, k, t, 0.0, iv)),
                call_iv: Some(iv),
                put_iv: Some(iv),
            })
            .collect();
        let chain = ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        };
        let strip = build_strip(&chain).unwrap();
        assert_eq!(strip.quotes.len(), DENSE_GRID_POINTS);
        assert!((strip.quotes.first().unwrap().strike - 10.0).abs() < 1e-9);
        assert!((strip.quotes.last().unwrap().strike - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn single_side_wings_still_build_via_iv_fallback() {
        // Top of the grid has only call IVs (no put), bottom has only
        // put IVs. As long as ≥ MIN_STRIP_QUOTES strikes carry a
        // usable IV (call OR put) and at least one strike has both
        // legs quoted for the forward picker, the build succeeds.
        let t = 0.25;
        let iv = 0.5;
        let f = 100.0;
        // ATM legs with both sides quoted — gives the forward picker.
        let mut legs = vec![ChainLeg {
            strike: f,
            call_mid_usd: Some(call_price(f, f, t, 0.0, iv)),
            put_mid_usd: Some(put_price(f, f, t, 0.0, iv)),
            call_iv: Some(iv),
            put_iv: Some(iv),
        }];
        // Lower wing — put-side only IV (call is gone).
        for k in [80.0, 85.0, 90.0, 95.0] {
            legs.push(ChainLeg {
                strike: k,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: None,
                put_iv: Some(iv),
            });
        }
        // Upper wing — call-side only IV.
        for k in [105.0, 110.0, 115.0, 120.0] {
            legs.push(ChainLeg {
                strike: k,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(iv),
                put_iv: None,
            });
        }
        let chain = ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        };
        let strip = build_strip(&chain).unwrap();
        assert!((strip.forward - f).abs() < 1e-9);
        assert_eq!(strip.quotes.len(), DENSE_GRID_POINTS);
    }

    #[test]
    fn rejects_when_forward_is_outside_listed_strike_range() {
        // Construct a chain where every strike sits well below the
        // implied forward — picker yields F outside [K_min, K_max] →
        // §4.3 step 3 rejects rather than extrapolating.
        let t = 0.25;
        let iv = 0.5;
        // K's all below 50; the only two-sided leg pins F via a large
        // C − P bias to push F above K_max.
        let legs = vec![
            ChainLeg {
                strike: 10.0,
                call_mid_usd: Some(60.0),
                put_mid_usd: Some(1.0),
                call_iv: Some(iv),
                put_iv: Some(iv),
            },
            ChainLeg {
                strike: 20.0,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(iv),
                put_iv: Some(iv),
            },
            ChainLeg {
                strike: 30.0,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(iv),
                put_iv: Some(iv),
            },
            ChainLeg {
                strike: 40.0,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(iv),
                put_iv: Some(iv),
            },
            ChainLeg {
                strike: 50.0,
                call_mid_usd: None,
                put_mid_usd: None,
                call_iv: Some(iv),
                put_iv: Some(iv),
            },
        ];
        let chain = ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        };
        match build_strip(&chain) {
            Err(BuildError::ForwardOutsideStrikeRange { forward, .. }) => {
                assert!(forward > 50.0, "F={forward} should overshoot K_max=50");
            }
            other => panic!("expected ForwardOutsideStrikeRange, got {other:?}"),
        }
    }

    #[test]
    fn put_call_parity_holds_for_strip_split_point() {
        // At K = F, C − P = 0 (r = 0). Average then equals each leg.
        let chain = flat_iv_chain(100.0, 5.0, 10, 0.25, 0.5);
        let strip = build_strip(&chain).unwrap();
        let atm = strip
            .quotes
            .iter()
            .find(|q| (q.strike - 100.0).abs() < 1e-9)
            .unwrap();
        let p = put_price(strip.forward, 100.0, 0.25, 0.0, atm.iv);
        let c = call_price(strip.forward, 100.0, 0.25, 0.0, atm.iv);
        assert!((p - c).abs() < 1e-9); // F = K, parity gives equality
        assert!((atm.q_usd - p).abs() < 1e-9);
    }
}
