//! Black-Scholes pricing on the forward (per `METHODOLOGY.md` §4.5).
//!
//! Inputs and outputs are USD; `r = 0` per §4.4, but the parameter is kept
//! in the signature so unit tests can sanity-check non-zero rate cases.
//!
//! Formulas (forward form):
//!
//! ```text
//! d1 = (ln(F/K) + 0.5 σ² T) / (σ √T)
//! d2 = d1 − σ √T
//! call = e^{−rT} · ( F · N(d1) − K · N(d2) )
//! put  = e^{−rT} · ( K · N(−d2) − F · N(−d1) )
//! N(x) = 0.5 · (1 + erf(x / √2))
//! ```
//!
//! `libm::erf` is used because `f64::erf` is still nightly-only.

use std::f64::consts::FRAC_1_SQRT_2;

/// Standard normal CDF.
#[inline]
fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x * FRAC_1_SQRT_2))
}

/// Black-Scholes call price on the forward.
///
/// Edge cases:
/// - `t <= 0` or `iv <= 0` → intrinsic `max(F − K, 0) · e^{−rT}` (the
///   variance integral never asks for this, but the BS function is
///   defined on the whole positive quadrant so an upstream rounding
///   that produces `t == 0` does not blow up).
/// - `f <= 0` or `k <= 0` → returns `0.0`. The variance integral filters
///   strikes against this anyway; the helper just refuses to produce a
///   negative or NaN price.
#[must_use]
pub fn call_price(f: f64, k: f64, t: f64, r: f64, iv: f64) -> f64 {
    if !(f > 0.0 && k > 0.0) {
        return 0.0;
    }
    if t <= 0.0 || iv <= 0.0 {
        return ((f - k).max(0.0)) * (-r * t).exp();
    }
    let sigma_sqrt_t = iv * t.sqrt();
    let d1 = ((f / k).ln() + 0.5 * iv * iv * t) / sigma_sqrt_t;
    let d2 = d1 - sigma_sqrt_t;
    (f * norm_cdf(d1) - k * norm_cdf(d2)) * (-r * t).exp()
}

/// Black-Scholes put price on the forward. Same edge cases as
/// [`call_price`]; intrinsic is `max(K − F, 0) · e^{−rT}`.
#[must_use]
pub fn put_price(f: f64, k: f64, t: f64, r: f64, iv: f64) -> f64 {
    if !(f > 0.0 && k > 0.0) {
        return 0.0;
    }
    if t <= 0.0 || iv <= 0.0 {
        return ((k - f).max(0.0)) * (-r * t).exp();
    }
    let sigma_sqrt_t = iv * t.sqrt();
    let d1 = ((f / k).ln() + 0.5 * iv * iv * t) / sigma_sqrt_t;
    let d2 = d1 - sigma_sqrt_t;
    (k * norm_cdf(-d2) - f * norm_cdf(-d1)) * (-r * t).exp()
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // intentional exact-bit assertions on intrinsic identities
mod tests {
    use super::*;

    /// ATM identity: at `F = K`, `call − put = 0` (put-call parity with
    /// `r = 0`). A canonical no-arb invariant.
    #[test]
    fn atm_call_equals_atm_put() {
        let (f, k, t, iv) = (100.0, 100.0, 0.25, 0.5);
        let c = call_price(f, k, t, 0.0, iv);
        let p = put_price(f, k, t, 0.0, iv);
        assert!((c - p).abs() < 1e-12, "c={c}, p={p}");
    }

    /// Put-call parity with general K (zero rate): `C − P = F − K`.
    #[test]
    fn put_call_parity_zero_rate() {
        for k in [80.0, 100.0, 120.0, 150.0] {
            let c = call_price(100.0, k, 0.5, 0.0, 0.6);
            let p = put_price(100.0, k, 0.5, 0.0, 0.6);
            let lhs = c - p;
            let rhs = 100.0 - k;
            assert!((lhs - rhs).abs() < 1e-9, "K={k}: c-p={lhs} F-K={rhs}");
        }
    }

    /// Put-call parity at non-zero rate: `C − P = e^{−rT}(F − K)`.
    #[test]
    fn put_call_parity_nonzero_rate() {
        let (f, k, t, r, iv) = (100.0, 110.0, 0.5, 0.05, 0.4);
        let c = call_price(f, k, t, r, iv);
        let p = put_price(f, k, t, r, iv);
        let expected = (-r * t).exp() * (f - k);
        assert!((c - p - expected).abs() < 1e-9);
    }

    /// Far-OTM call value approaches zero. Cheap smoke that `d1`, `d2`
    /// signs are correct.
    #[test]
    fn deep_otm_call_is_tiny() {
        let c = call_price(100.0, 1000.0, 0.25, 0.0, 0.4);
        assert!(c < 1e-3, "deep-OTM call too rich: {c}");
    }

    /// `t == 0` reduces to intrinsic.
    #[test]
    fn zero_t_is_intrinsic() {
        assert_eq!(call_price(100.0, 80.0, 0.0, 0.0, 0.4), 20.0);
        assert_eq!(put_price(100.0, 120.0, 0.0, 0.0, 0.4), 20.0);
    }

    /// `iv == 0` also reduces to intrinsic (the BS limit). Important
    /// because the clamp lower bound is `1e-4`, but the function should
    /// behave on the singular case rather than NaN.
    #[test]
    fn zero_iv_is_intrinsic() {
        assert_eq!(call_price(100.0, 80.0, 0.5, 0.0, 0.0), 20.0);
    }

    /// `norm_cdf` known values.
    #[test]
    fn norm_cdf_canonical_values() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-15);
        assert!((norm_cdf(1.0) - 0.841_344_746).abs() < 1e-7);
        assert!((norm_cdf(-1.0) - 0.158_655_254).abs() < 1e-7);
    }
}
