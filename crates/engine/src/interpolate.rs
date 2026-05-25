//! 30-day constant-maturity interpolation in total-variance space
//! (issue #19, `METHODOLOGY.md` §4.6 + §4.7).
//!
//! Formula (working in **minutes** to avoid fractional-year rounding):
//!
//! ```text
//! N₃₀  = 30  · 1440
//! N₃₆₅ = 365 · 1440
//! T₁   = N_T₁ / N₃₆₅                   (years, near expiry)
//! T₂   = N_T₂ / N₃₆₅                   (years, next expiry)
//! w₁ = (N_T₂ − N₃₀) / (N_T₂ − N_T₁)
//! w₂ = (N₃₀  − N_T₁) / (N_T₂ − N_T₁)
//! σ²_30d = (T₁ · σ²₁ · w₁ + T₂ · σ²₂ · w₂) · (N₃₆₅ / N₃₀)
//! ```
//!
//! `σ²₁` and `σ²₂` are the *annualised* per-expiry variances returned by
//! [`crate::variance::variance_t`]. The interpolation multiplies by `T`
//! itself to move into total-variance space — the variance function
//! does **not** pre-multiply (HIGH-1 from PR #52's review).
//!
//! §4.7 conversion: `BVOL = 100 · √σ²_30d`. Output is in volatility
//! points (e.g. `65.40`), not a decimal fraction.
//!
//! Reject on:
//! - `N_T₁ >= N_T₂` (degenerate or inverted bracket)
//! - `σ²_30d < 0` (§4.6 explicit; should be unreachable with the §4.1
//!   expiry picker that guarantees `N_T₁ <= N₃₀ <= N_T₂`)
//! - non-finite result

use volx_shared_types::units::{Minutes, Years};

/// One side of the interpolation: annualised variance + time-to-expiry
/// in minutes. The minutes form is what §4.6 actually consumes — keeping
/// it explicit here avoids a `Years → Minutes` conversion that would
/// reintroduce the rounding §4.6 is written in minutes to avoid.
#[derive(Debug, Clone, Copy)]
pub struct ExpiryVariance {
    /// Annualised variance `σ²_T` (units `1 / year`).
    pub sigma_sq: f64,
    /// Time to expiry in minutes (the §4.6 `N_T` quantity).
    pub n_t: Minutes,
}

impl ExpiryVariance {
    /// Convenience: take `T` in years and convert to minutes.
    #[must_use]
    pub fn from_years(sigma_sq: f64, t: Years) -> Self {
        Self {
            sigma_sq,
            n_t: t.to_minutes(),
        }
    }
}

/// Why the interpolation can fail. Non-fatal at the engine level — the
/// scheduler (#20) records the reason on the published row.
#[derive(Debug, thiserror::Error)]
pub enum InterpError {
    #[error("near n_t = {0} must be finite and > 0")]
    NonPositiveNearNt(f64),
    #[error("next n_t = {0} must be finite and > 0")]
    NonPositiveNextNt(f64),
    #[error(
        "expiries do not bracket 30d: near n_t = {near} must be < next n_t = {next} (and ideally near ≤ N₃₀ ≤ next per §4.1)"
    )]
    InvertedBracket { near: f64, next: f64 },
    #[error("near σ² = {0} must be finite and non-negative")]
    NonFiniteNearVariance(f64),
    #[error("next σ² = {0} must be finite and non-negative")]
    NonFiniteNextVariance(f64),
    #[error(
        "interpolated σ²_30d = {0} < 0; §4.6 rejects (expiry pair likely violates `near ≤ N₃₀ ≤ next`)"
    )]
    NegativeVariance(f64),
    #[error("interpolated σ²_30d = {0} is non-finite (numeric overflow)")]
    NonFiniteResult(f64),
}

/// 30-day annualised variance from the near + next expiries (§4.6).
///
/// Returns `σ²_30d` in units of `1 / year`. Multiply by `N₃₀/N₃₆₅` to get
/// total variance at the 30-day horizon (most callers want the
/// annualised form, which is what [`bvol`] consumes).
pub fn interpolate_30d(near: ExpiryVariance, next: ExpiryVariance) -> Result<f64, InterpError> {
    let n_t1 = near.n_t.0;
    let n_t2 = next.n_t.0;

    if !n_t1.is_finite() || n_t1 <= 0.0 {
        return Err(InterpError::NonPositiveNearNt(n_t1));
    }
    if !n_t2.is_finite() || n_t2 <= 0.0 {
        return Err(InterpError::NonPositiveNextNt(n_t2));
    }
    if n_t1 >= n_t2 {
        return Err(InterpError::InvertedBracket {
            near: n_t1,
            next: n_t2,
        });
    }
    if !near.sigma_sq.is_finite() || near.sigma_sq < 0.0 {
        return Err(InterpError::NonFiniteNearVariance(near.sigma_sq));
    }
    if !next.sigma_sq.is_finite() || next.sigma_sq < 0.0 {
        return Err(InterpError::NonFiniteNextVariance(next.sigma_sq));
    }

    let n_30 = Minutes::N_30D.0;
    let n_365 = Minutes::N_365D.0;
    let t1 = n_t1 / n_365;
    let t2 = n_t2 / n_365;

    // Weights are linear in N_T-space. Sign rule:
    // - When n_t1 < N_30 < n_t2 (the §4.1 happy path), both weights
    //   are in (0, 1) and the formula is a convex blend.
    // - When n_t1 == N_30, w1 = 1 and w2 = 0; result collapses to σ²₁.
    // - When n_t2 == N_30, w1 = 0 and w2 = 1; result collapses to σ²₂.
    // - When N_30 is outside [n_t1, n_t2] (extrapolation), one weight
    //   goes negative; we keep computing and rely on the
    //   `NegativeVariance` rejection at the end — the §4.1 picker
    //   should never feed an out-of-bracket pair, so a hit here is a
    //   bug-signal worth surfacing.
    let denom = n_t2 - n_t1;
    let w1 = (n_t2 - n_30) / denom;
    let w2 = (n_30 - n_t1) / denom;

    let total_30d = (t1 * near.sigma_sq * w1 + t2 * next.sigma_sq * w2) * (n_365 / n_30);

    if !total_30d.is_finite() {
        return Err(InterpError::NonFiniteResult(total_30d));
    }
    if total_30d < 0.0 {
        return Err(InterpError::NegativeVariance(total_30d));
    }
    Ok(total_30d)
}

/// §4.7: `BVOL = 100 · √σ²_30d`. Output in volatility points (e.g. `65.40`).
///
/// Returns `None` if `sigma_sq_30d` is non-finite or negative — these
/// shapes are filtered upstream by [`interpolate_30d`], so a `None`
/// here indicates a programmer who skipped the interpolation guard.
#[must_use]
pub fn bvol(sigma_sq_30d: f64) -> Option<f64> {
    if !sigma_sq_30d.is_finite() || sigma_sq_30d < 0.0 {
        return None;
    }
    Some(100.0 * sigma_sq_30d.sqrt())
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // intentional comparisons against algebraic identities
mod tests {
    use super::*;

    /// `N_T₁ == N₃₀` exactly: the formula must collapse to σ²₁ regardless
    /// of σ²₂. Catches a sign / weight bug at the left boundary.
    #[test]
    fn near_at_30d_collapses_to_near_variance() {
        let near = ExpiryVariance {
            sigma_sq: 0.36,
            n_t: Minutes::N_30D,
        };
        let next = ExpiryVariance {
            sigma_sq: 0.99, // intentionally different — must be ignored
            n_t: Minutes(60.0 * 1440.0),
        };
        let s30 = interpolate_30d(near, next).unwrap();
        assert!((s30 - 0.36).abs() < 1e-12, "got {s30}");
    }

    /// `N_T₂ == N₃₀` exactly: collapses to σ²₂.
    #[test]
    fn next_at_30d_collapses_to_next_variance() {
        let near = ExpiryVariance {
            sigma_sq: 0.99,
            n_t: Minutes(7.0 * 1440.0),
        };
        let next = ExpiryVariance {
            sigma_sq: 0.36,
            n_t: Minutes::N_30D,
        };
        let s30 = interpolate_30d(near, next).unwrap();
        assert!((s30 - 0.36).abs() < 1e-12, "got {s30}");
    }

    /// Equal IV on both expiries: the interpolated `σ²_30d` equals that
    /// shared σ² — convex blend of equal values is itself.
    #[test]
    fn equal_variance_passes_through() {
        let near = ExpiryVariance {
            sigma_sq: 0.5,
            n_t: Minutes(15.0 * 1440.0),
        };
        let next = ExpiryVariance {
            sigma_sq: 0.5,
            n_t: Minutes(45.0 * 1440.0),
        };
        let s30 = interpolate_30d(near, next).unwrap();
        assert!((s30 - 0.5).abs() < 1e-12);
    }

    /// `BVOL = 100 · √σ²`. σ² = 0.36 → BVOL = 60.0.
    #[test]
    fn bvol_conversion_known_value() {
        assert_eq!(bvol(0.36), Some(60.0));
        assert_eq!(bvol(0.0), Some(0.0));
        assert_eq!(bvol(1.0), Some(100.0));
    }

    #[test]
    fn bvol_rejects_negative() {
        assert_eq!(bvol(-1.0), None);
        assert_eq!(bvol(f64::NAN), None);
        assert_eq!(bvol(f64::INFINITY), None);
    }

    /// Full pipeline smoke: σ²₁ = 0.40, σ²₂ = 0.50 at 14d + 60d.
    /// Sanity-check the result is bracketed by the two endpoint
    /// variances and increasing with the next-leg weight.
    #[test]
    fn midpoint_interpolation_is_bracketed() {
        let near = ExpiryVariance::from_years(0.40, Years(14.0 / 365.0));
        let next = ExpiryVariance::from_years(0.50, Years(60.0 / 365.0));
        let s30 = interpolate_30d(near, next).unwrap();
        assert!((0.40..=0.50).contains(&s30), "got {s30}");
    }

    /// Weights sum to 1 (`w₁ + w₂ = 1`): bake the algebraic identity
    /// into a test so a future refactor of the weight formulas
    /// can't drift unnoticed. Done indirectly by checking equal-σ²
    /// passthrough at an asymmetric bracket.
    #[test]
    fn weights_sum_to_one_asymmetric_bracket() {
        let near = ExpiryVariance::from_years(0.7, Years(10.0 / 365.0));
        let next = ExpiryVariance::from_years(0.7, Years(60.0 / 365.0));
        let s30 = interpolate_30d(near, next).unwrap();
        assert!((s30 - 0.7).abs() < 1e-12, "got {s30}");
    }

    #[test]
    fn rejects_inverted_bracket() {
        let near = ExpiryVariance::from_years(0.5, Years(60.0 / 365.0));
        let next = ExpiryVariance::from_years(0.5, Years(14.0 / 365.0));
        assert!(matches!(
            interpolate_30d(near, next),
            Err(InterpError::InvertedBracket { .. })
        ));
    }

    #[test]
    fn rejects_degenerate_equal_expiries() {
        let near = ExpiryVariance::from_years(0.5, Years(30.0 / 365.0));
        let next = ExpiryVariance::from_years(0.5, Years(30.0 / 365.0));
        assert!(matches!(
            interpolate_30d(near, next),
            Err(InterpError::InvertedBracket { .. })
        ));
    }

    #[test]
    fn rejects_non_positive_near_nt() {
        let near = ExpiryVariance {
            sigma_sq: 0.5,
            n_t: Minutes(0.0),
        };
        let next = ExpiryVariance::from_years(0.5, Years(60.0 / 365.0));
        assert!(matches!(
            interpolate_30d(near, next),
            Err(InterpError::NonPositiveNearNt(_))
        ));
    }

    #[test]
    fn rejects_non_finite_variance() {
        let near = ExpiryVariance {
            sigma_sq: f64::NAN,
            n_t: Minutes(15.0 * 1440.0),
        };
        let next = ExpiryVariance::from_years(0.5, Years(60.0 / 365.0));
        assert!(matches!(
            interpolate_30d(near, next),
            Err(InterpError::NonFiniteNearVariance(_))
        ));
    }

    /// Out-of-bracket extrapolation produces a negative variance
    /// (§4.1 picker should prevent this; the guard is defence-in-depth).
    /// Construct: both expiries > 30d → `N₃₀ < n_t1` → `w₁ > 1`, `w₂ < 0`;
    /// σ²₂ ≫ σ²₁ so the `w₂ · T₂ · σ²₂` term dominates and forces the
    /// sum negative.
    #[test]
    fn rejects_extrapolation_above_30d() {
        let near = ExpiryVariance::from_years(0.10, Years(45.0 / 365.0));
        let next = ExpiryVariance::from_years(5.0, Years(60.0 / 365.0));
        match interpolate_30d(near, next) {
            Err(InterpError::NegativeVariance(_)) => {}
            other => panic!("expected NegativeVariance, got {other:?}"),
        }
    }
}
