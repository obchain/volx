//! Per-expiry variance integral (issue #18, `METHODOLOGY.md` §4.5).
//!
//! Trapezoidal Carr-Madan on the fitted-IV dense grid produced by
//! [`crate::strip::build_strip`]. Pure numerics: input is a [`Strip`],
//! output is the per-expiry total variance `σ²_T` (already multiplied
//! by `T`, since the 30-day interp in §4.6 operates in total-variance
//! space — the caller divides by `T` to recover the annualised
//! variance when it needs vol).
//!
//! Formula (§4.5):
//!
//! ```text
//! σ²_T = (2 e^{rT} / T) · ∫ Q(K)/K² dK   −   (F/K₀ − 1)² / T
//! ```
//!
//! The integral is taken over the dense grid `[K_min, K_max]`. No
//! wing extrapolation — `bvol-dvol-gap-diagnostics.ipynb` §4 showed
//! that pushing the IV surface past the listed range *increases*
//! BVOL-vs-DVOL gap rather than tightening it.
//!
//! Returns `Err(VarianceError::NegativeVariance)` if the result is
//! negative. With a healthy fitted-IV surface this never happens; if
//! it does, the upstream data is bad and the scheduler (#20) tags the
//! snapshot as rejected rather than publishing a nonsense value.

use volx_shared_types::strip::{MIN_STRIP_QUOTES, Strip};

/// Why a variance computation can fail. Non-fatal at the engine level —
/// the scheduler (#20) records the reason on the published row and
/// continues with the next snapshot.
#[derive(Debug, thiserror::Error)]
pub enum VarianceError {
    #[error(
        "strip has {0} quotes, below MIN_STRIP_QUOTES={MIN_STRIP_QUOTES}; should be unreachable (Strip's deserializer rejects), guarded as a bug fence"
    )]
    TooFewQuotes(usize),
    #[error("strip time-to-expiry T = {0} is not finite or not positive")]
    NonPositiveT(f64),
    #[error("strip k_zero = {0} is not finite or not positive")]
    NonPositiveKZero(f64),
    #[error(
        "trapezoidal integrand is non-finite at index {idx} (K={strike}, Q={q_usd}); upstream data is bad"
    )]
    NonFiniteIntegrand { idx: usize, strike: f64, q_usd: f64 },
    #[error(
        "Carr-Madan integral produced σ²_T = {0} < 0; §4.5 rejects (upstream IV surface is broken)"
    )]
    NegativeVariance(f64),
}

/// Compute the per-expiry total variance `σ²_T` for the supplied strip
/// using `r = 0` per §4.4.
pub fn variance_t(strip: &Strip) -> Result<f64, VarianceError> {
    variance_t_with_rate(strip, 0.0)
}

/// Variant taking an explicit `r` — used in tests and reserved for the
/// future §4.4 bump to a USDC/USDT lending rate.
pub fn variance_t_with_rate(strip: &Strip, r: f64) -> Result<f64, VarianceError> {
    let t = strip.time_to_expiry.0;
    if !t.is_finite() || t <= 0.0 {
        return Err(VarianceError::NonPositiveT(t));
    }
    if !strip.k_zero.is_finite() || strip.k_zero <= 0.0 {
        return Err(VarianceError::NonPositiveKZero(strip.k_zero));
    }
    if strip.quotes.len() < MIN_STRIP_QUOTES {
        return Err(VarianceError::TooFewQuotes(strip.quotes.len()));
    }

    // Trapezoidal sum of f(K) = Q(K) / K² over the (non-uniform-tolerant)
    // dense grid. The strip builder uses a uniform grid today, but
    // accepting a non-uniform spacing here costs nothing and decouples
    // the integrator from the grid generator.
    let mut integral = 0.0_f64;
    for (idx, win) in strip.quotes.windows(2).enumerate() {
        let (a, b) = (&win[0], &win[1]);
        let fa = a.q_usd / (a.strike * a.strike);
        let fb = b.q_usd / (b.strike * b.strike);
        if !fa.is_finite() {
            return Err(VarianceError::NonFiniteIntegrand {
                idx,
                strike: a.strike,
                q_usd: a.q_usd,
            });
        }
        if !fb.is_finite() {
            return Err(VarianceError::NonFiniteIntegrand {
                idx: idx + 1,
                strike: b.strike,
                q_usd: b.q_usd,
            });
        }
        let dk = b.strike - a.strike;
        integral += 0.5 * (fa + fb) * dk;
    }

    let growth = (r * t).exp();
    let leading = 2.0 * growth / t * integral;
    let correction = {
        let ratio = strip.forward / strip.k_zero - 1.0;
        ratio * ratio / t
    };
    let sigma_sq_t = leading - correction;

    if !sigma_sq_t.is_finite() {
        return Err(VarianceError::NonFiniteIntegrand {
            idx: 0,
            strike: f64::NAN,
            q_usd: f64::NAN,
        });
    }
    if sigma_sq_t < 0.0 {
        return Err(VarianceError::NegativeVariance(sigma_sq_t));
    }

    // §4.5 returns *annualised* variance σ²_T; the caller multiplies by
    // T to recover total variance when working in §4.6's interpolation
    // space. Naming-wise this is "sigma squared (annualised)" — the
    // `_t` suffix in the issue body refers to "per-expiry" not "× T".
    Ok(sigma_sq_t)
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // intentional comparisons against analytic baselines
mod tests {
    use super::*;
    use crate::bs::{call_price, put_price};
    use crate::strip::{ChainLeg, ExpiryChain, build_strip};
    use volx_shared_types::strip::{MIN_FITTED_IV, StripQuote};
    use volx_shared_types::units::Years;

    /// Build a synthetic flat-IV chain centered on `forward`, then run
    /// the strip builder. Replicates the `flat_iv_chain` helper from
    /// `strip::tests` so this module's tests don't reach across crate
    /// boundaries.
    fn flat_iv_strip(forward: f64, step: f64, n_pairs: usize, t: f64, iv: f64) -> Strip {
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
        let chain = ExpiryChain {
            time_to_expiry: Years(t),
            legs,
        };
        build_strip(&chain).unwrap()
    }

    /// Constant-IV chain: Carr-Madan replication recovers `iv²` exactly
    /// in the limit of an infinitely wide, infinitely dense grid. For a
    /// finite grid with strikes inside `±n·σ·√T` we should still hit
    /// `iv²` to a few % — the trunc error is bounded by the wings.
    ///
    /// This test uses a wide grid (±10σ) so wing truncation is well
    /// below 1 %. Tighter tolerances on tighter grids belong to the
    /// Python-reference parity test (issue #21) — this is the smoke
    /// check that the integrator is wired up correctly.
    #[test]
    fn flat_iv_recovers_variance_within_one_percent() {
        let (forward, t, iv): (f64, f64, f64) = (100.0, 0.25, 0.6);
        let sigma_t_strike = iv * forward * t.sqrt(); // BS 1-σ move
        let half_width = 10.0 * sigma_t_strike;
        let step = half_width / 50.0; // 101 listed strikes
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n_pairs = (half_width / step) as usize;
        let strip = flat_iv_strip(forward, step, n_pairs, t, iv);

        let sigma_sq = variance_t(&strip).unwrap();
        let expected = iv * iv;
        let rel_err = ((sigma_sq - expected) / expected).abs();
        assert!(
            rel_err < 1.0e-2,
            "σ² = {sigma_sq}, expected ≈ {expected} (rel_err={rel_err})"
        );
    }

    /// Same setup, doubled IV: the recovery should scale with `iv²`.
    #[test]
    fn flat_iv_recovers_variance_at_higher_iv() {
        let (forward, t, iv): (f64, f64, f64) = (100.0, 0.25, 1.2);
        let sigma_t_strike = iv * forward * t.sqrt();
        let half_width = 10.0 * sigma_t_strike;
        let step = half_width / 50.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n_pairs = (half_width / step) as usize;
        let strip = flat_iv_strip(forward, step, n_pairs, t, iv);

        let sigma_sq = variance_t(&strip).unwrap();
        let expected = iv * iv;
        let rel_err = ((sigma_sq - expected) / expected).abs();
        assert!(rel_err < 1.0e-2, "σ² = {sigma_sq}, expected ≈ {expected}");
    }

    /// Variance scales monotonically with IV (square-law): doubling
    /// IV must roughly quadruple σ². Cheap algebraic sanity check that
    /// neither the trapezoidal sum nor the F/K₀ correction is silently
    /// rescaling the result.
    #[test]
    fn variance_scales_quadratically_with_iv() {
        let (forward, t): (f64, f64) = (100.0, 0.25);
        let strip_lo = flat_iv_strip(forward, 4.0, 30, t, 0.4);
        let strip_hi = flat_iv_strip(forward, 4.0, 30, t, 0.8);
        let sigma_lo = variance_t(&strip_lo).unwrap();
        let sigma_hi = variance_t(&strip_hi).unwrap();
        let ratio = sigma_hi / sigma_lo;
        // Ratio should be ~4 (0.8² / 0.4²). Wing-truncation introduces
        // a few % bias, so accept 3.5 < ratio < 4.5.
        assert!(
            (3.5..=4.5).contains(&ratio),
            "σ²_hi / σ²_lo = {ratio}, expected ≈ 4"
        );
    }

    #[test]
    fn rejects_non_positive_t() {
        let mut strip = flat_iv_strip(100.0, 4.0, 10, 0.25, 0.5);
        strip.time_to_expiry = Years(0.0);
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::NonPositiveT(_))
        ));
        strip.time_to_expiry = Years(-1.0);
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::NonPositiveT(_))
        ));
    }

    #[test]
    fn rejects_non_positive_k_zero() {
        let mut strip = flat_iv_strip(100.0, 4.0, 10, 0.25, 0.5);
        strip.k_zero = 0.0;
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::NonPositiveKZero(_))
        ));
    }

    #[test]
    fn rejects_too_few_quotes() {
        let strip = Strip {
            forward: 100.0,
            k_zero: 100.0,
            time_to_expiry: Years(0.25),
            quotes: (0..3_u32)
                .map(|i| StripQuote {
                    strike: 90.0 + f64::from(i) * 10.0,
                    q_usd: 1.0,
                    iv: MIN_FITTED_IV,
                })
                .collect(),
        };
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::TooFewQuotes(3))
        ));
    }

    /// Inject `Inf` into one quote's `q_usd` and confirm the integrator
    /// surfaces the non-finite condition rather than producing `NaN`
    /// σ².
    #[test]
    fn rejects_non_finite_integrand() {
        let mut strip = flat_iv_strip(100.0, 4.0, 10, 0.25, 0.5);
        strip.quotes[3].q_usd = f64::INFINITY;
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::NonFiniteIntegrand { .. })
        ));
    }

    /// Synthesize a strip whose integral is small enough that the
    /// `(F/K₀ − 1)² / T` correction dominates and pushes the result
    /// negative. Catches the §4.5 rejection clause.
    #[test]
    fn rejects_negative_variance() {
        // Zero-everywhere quote vector → integral = 0, so
        // σ²_T = − (F/K₀ − 1)² / T. With F ≠ K₀ this is < 0.
        let strip = Strip {
            forward: 100.0,
            k_zero: 80.0, // F/K0 = 1.25, ratio ≠ 0
            time_to_expiry: Years(0.25),
            quotes: (0..MIN_STRIP_QUOTES)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let k = 90.0 + i as f64 * 5.0;
                    StripQuote {
                        strike: k,
                        q_usd: 0.0,
                        iv: MIN_FITTED_IV,
                    }
                })
                .collect(),
        };
        assert!(matches!(
            variance_t(&strip),
            Err(VarianceError::NegativeVariance(_))
        ));
    }

    /// At `F == K₀` exactly, the correction term is zero and the
    /// trapezoidal sum dominates. Smoke that the correction algebra
    /// doesn't introduce a stray factor on the equality case.
    #[test]
    fn zero_correction_when_forward_equals_k_zero() {
        let mut strip = flat_iv_strip(100.0, 4.0, 10, 0.25, 0.5);
        strip.k_zero = strip.forward;
        let sigma_sq = variance_t(&strip).unwrap();
        assert!(sigma_sq > 0.0);
    }
}
