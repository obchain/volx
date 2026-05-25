//! Natural cubic spline interpolation (per `METHODOLOGY.md` §4.3 step 3).
//!
//! "Natural" here is the standard boundary condition: second derivative
//! at the endpoints is zero. This matches `scipy.interpolate.CubicSpline`
//! with `bc_type='natural'`, which is what the Python reference uses.
//!
//! ### Why hand-rolled
//!
//! Pulling in a heavy interpolation crate would bring numerical algebra
//! deps the engine does not otherwise need. The natural cubic spline fits
//! in ~70 lines and the Thomas-algorithm tridiagonal solve is `O(n)` —
//! cheaper than any generic dispatch a crate would add.
//!
//! ### No extrapolation
//!
//! `eval()` returns `None` outside the data range. METHODOLOGY §4.3 step 3
//! is explicit: the strip builder must not extrapolate past `[K_min, K_max]`,
//! and `bvol-dvol-gap-diagnostics.ipynb` §4 showed that wing extrapolation
//! *increases* BVOL-vs-DVOL gap rather than tightening it.

/// Natural cubic spline over `(x_i, y_i)` knot points.
///
/// `xs` must be strictly increasing and have the same length as `ys`;
/// any other shape returns `Err`.
#[derive(Debug, Clone)]
pub struct NaturalCubicSpline {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Second derivative at each knot. `m[i]` corresponds to `xs[i]`;
    /// `m[0] = m[n-1] = 0` for the natural boundary condition.
    m: Vec<f64>,
}

/// Reasons spline construction can fail.
#[derive(Debug, thiserror::Error)]
pub enum SplineError {
    #[error("need at least 2 knots, got {0}")]
    TooFewKnots(usize),
    #[error("xs and ys length mismatch: {xs} vs {ys}")]
    LengthMismatch { xs: usize, ys: usize },
    #[error("xs must be strictly increasing (index {i}: x[{i}]={left} ≥ x[{}]={right})", i + 1)]
    NotStrictlyIncreasing { i: usize, left: f64, right: f64 },
    #[error("xs[{0}] = {1} is not finite")]
    NonFiniteX(usize, f64),
    #[error("ys[{0}] = {1} is not finite")]
    NonFiniteY(usize, f64),
}

impl NaturalCubicSpline {
    /// Fit a natural cubic spline through the supplied knots.
    pub fn fit(xs: &[f64], ys: &[f64]) -> Result<Self, SplineError> {
        if xs.len() < 2 {
            return Err(SplineError::TooFewKnots(xs.len()));
        }
        if xs.len() != ys.len() {
            return Err(SplineError::LengthMismatch {
                xs: xs.len(),
                ys: ys.len(),
            });
        }
        for (i, &x) in xs.iter().enumerate() {
            if !x.is_finite() {
                return Err(SplineError::NonFiniteX(i, x));
            }
        }
        for (i, &y) in ys.iter().enumerate() {
            if !y.is_finite() {
                return Err(SplineError::NonFiniteY(i, y));
            }
        }
        for i in 0..xs.len() - 1 {
            if xs[i] >= xs[i + 1] {
                return Err(SplineError::NotStrictlyIncreasing {
                    i,
                    left: xs[i],
                    right: xs[i + 1],
                });
            }
        }

        let n = xs.len();
        let mut m = vec![0.0_f64; n];

        // For n == 2 the natural spline degenerates to a straight line:
        // both endpoint second-derivatives are zero and there is no
        // interior unknown to solve for.
        if n > 2 {
            // Build tridiagonal system for the interior second derivatives.
            // a[i] m[i-1] + b[i] m[i] + c[i] m[i+1] = d[i], 1 ≤ i ≤ n-2.
            let mut a = vec![0.0_f64; n];
            let mut b = vec![0.0_f64; n];
            let mut c = vec![0.0_f64; n];
            let mut d = vec![0.0_f64; n];
            for i in 1..n - 1 {
                let h_prev = xs[i] - xs[i - 1];
                let h_next = xs[i + 1] - xs[i];
                a[i] = h_prev;
                b[i] = 2.0 * (h_prev + h_next);
                c[i] = h_next;
                d[i] = 6.0 * ((ys[i + 1] - ys[i]) / h_next - (ys[i] - ys[i - 1]) / h_prev);
            }

            // Thomas algorithm: forward sweep, then back-substitute.
            for i in 2..n - 1 {
                let w = a[i] / b[i - 1];
                b[i] -= w * c[i - 1];
                d[i] -= w * d[i - 1];
            }
            // m[0] = m[n-1] = 0 (natural).
            for i in (1..n - 1).rev() {
                m[i] = (d[i] - c[i] * m[i + 1]) / b[i];
            }
        }

        Ok(Self {
            xs: xs.to_vec(),
            ys: ys.to_vec(),
            m,
        })
    }

    /// Evaluate the spline at `x`. Returns `None` if `x` is outside
    /// `[xs[0], xs[n-1]]` — natural-cubic extrapolation is unstable and
    /// the methodology explicitly forbids it.
    ///
    /// # Panics
    ///
    /// Does not panic on valid inputs; the unreachable arms in the
    /// segment-locate branch are guarded by the prior range check.
    #[must_use]
    pub fn eval(&self, x: f64) -> Option<f64> {
        if !x.is_finite() {
            return None;
        }
        let n = self.xs.len();
        if x < self.xs[0] || x > self.xs[n - 1] {
            return None;
        }
        // Binary search for the segment `[xs[i], xs[i+1]]` containing x.
        // `partition_point` returns the first index where xs[i] > x; we
        // want the segment ending at that index.
        let i = match self.xs.partition_point(|&xi| xi <= x) {
            0 => 0,               // x == xs[0]
            i if i == n => n - 2, // x == xs[n-1]
            i => i - 1,
        };

        let (x0, x1) = (self.xs[i], self.xs[i + 1]);
        let (y0, y1) = (self.ys[i], self.ys[i + 1]);
        let (m0, m1) = (self.m[i], self.m[i + 1]);
        let h = x1 - x0;
        let a = (x1 - x) / h;
        let b = (x - x0) / h;
        Some(a * y0 + b * y1 + ((a.powi(3) - a) * m0 + (b.powi(3) - b) * m1) * h * h / 6.0)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // intentional exact-bit assertions on boundary conditions
mod tests {
    use super::*;

    #[test]
    fn linear_data_interpolates_linearly() {
        // y = 2x + 3 → spline must reproduce the line exactly.
        let xs = [0.0_f64, 1.0, 2.0, 3.0, 4.0];
        let ys: Vec<f64> = xs.iter().map(|x| 2.0 * x + 3.0).collect();
        let s = NaturalCubicSpline::fit(&xs, &ys).unwrap();
        for &q in &[0.5_f64, 1.5, 2.5, 3.5] {
            let v = s.eval(q).unwrap();
            assert!((v - (2.0 * q + 3.0)).abs() < 1e-12, "x={q}, v={v}");
        }
    }

    #[test]
    fn passes_through_every_knot() {
        let xs = [0.0_f64, 1.0, 3.0, 7.0, 9.0];
        let ys = [0.5, 1.2, 0.9, 2.0, 1.5];
        let s = NaturalCubicSpline::fit(&xs, &ys).unwrap();
        for i in 0..xs.len() {
            let v = s.eval(xs[i]).unwrap();
            assert!((v - ys[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn natural_boundary_second_derivative_zero() {
        // m[0] and m[n-1] must be exactly zero for the natural BC.
        let xs = [0.0_f64, 1.0, 2.0, 3.0];
        let ys = [1.0, 4.0, 9.0, 16.0];
        let s = NaturalCubicSpline::fit(&xs, &ys).unwrap();
        assert_eq!(s.m[0], 0.0);
        assert_eq!(*s.m.last().unwrap(), 0.0);
    }

    #[test]
    fn rejects_extrapolation() {
        let xs = [0.0_f64, 1.0, 2.0];
        let ys = [0.0, 1.0, 4.0];
        let s = NaturalCubicSpline::fit(&xs, &ys).unwrap();
        assert_eq!(s.eval(-0.001), None);
        assert_eq!(s.eval(2.001), None);
        assert!(s.eval(0.0).is_some());
        assert!(s.eval(2.0).is_some());
    }

    #[test]
    fn two_knot_degenerate_is_linear() {
        let xs = [0.0_f64, 10.0];
        let ys = [1.0, 5.0];
        let s = NaturalCubicSpline::fit(&xs, &ys).unwrap();
        assert!((s.eval(2.5).unwrap() - 2.0).abs() < 1e-12);
        assert!((s.eval(7.5).unwrap() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn rejects_non_increasing_xs() {
        let xs = [0.0_f64, 1.0, 1.0, 2.0];
        let ys = [0.0; 4];
        assert!(NaturalCubicSpline::fit(&xs, &ys).is_err());
    }

    #[test]
    fn rejects_length_mismatch() {
        let xs = [0.0_f64, 1.0, 2.0];
        let ys = [0.0_f64, 1.0];
        assert!(NaturalCubicSpline::fit(&xs, &ys).is_err());
    }

    #[test]
    fn rejects_non_finite_inputs() {
        let xs = [0.0_f64, f64::NAN, 2.0];
        let ys = [0.0_f64, 1.0, 2.0];
        assert!(NaturalCubicSpline::fit(&xs, &ys).is_err());
    }
}
