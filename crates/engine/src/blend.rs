//! Cross-venue blend policy (issue #61, `math_reference.md:71`).
//!
//! Today: pure median over per-venue raw indices. Outlier-drop is a
//! follow-up policy (separate issue) that filters the input slice
//! before calling [`median`] — keeping the blend a free function over
//! a pre-filtered slice keeps the two policies composable.
//!
//! Median (not mean) is the methodology choice: a single venue with
//! fat-finger quotes or a stuck-tick outage cannot drag the published
//! index in either direction. CBOE applies the same policy on the SPX
//! cross-exchange feed; the methodology doc pins it for `VolX`.
//!
//! Single-venue degraded mode (e.g. OKX + Bybit down) collapses to
//! identity — `median([x]) == x` — so the engine keeps publishing
//! without a code branch.

/// Median over a slice of f64 values, NaN-safe.
///
/// - Empty slice → `None` (caller decides whether that's an error).
/// - Single element → that element (identity, for the degraded
///   single-venue path).
/// - Odd count → the middle value after sorting.
/// - Even count → arithmetic mean of the two middle values (standard
///   median convention).
///
/// Sorting uses [`f64::total_cmp`] so NaNs are deterministically
/// ordered (greater than every non-NaN). Callers should drop
/// non-finite values upstream — feeding NaNs through here will
/// produce a defined-but-meaningless answer.
#[must_use]
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n.is_multiple_of(2) {
        // Even — average the two middle values. Indices `n/2 - 1` and `n/2`.
        Some(f64::midpoint(sorted[n / 2 - 1], sorted[n / 2]))
    } else {
        Some(sorted[n / 2])
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_none() {
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn single_value_is_identity() {
        // Degraded single-venue path — must publish the lone value
        // unchanged so the engine keeps producing rows when 2 of 3
        // venues are down.
        assert_eq!(median(&[42.5]), Some(42.5));
    }

    #[test]
    fn three_values_picks_middle() {
        // Canonical odd-count median.
        assert_eq!(median(&[10.0, 20.0, 30.0]), Some(20.0));
        // Unsorted input: sort happens internally.
        assert_eq!(median(&[30.0, 10.0, 20.0]), Some(20.0));
    }

    #[test]
    fn three_values_outlier_ignored() {
        // The whole point: one venue at 1000 (fat finger) cannot
        // drag the index off the middle value.
        assert_eq!(median(&[50.0, 51.0, 1_000.0]), Some(51.0));
        assert_eq!(median(&[50.0, 51.0, -1_000.0]), Some(50.0));
    }

    #[test]
    fn even_count_is_mean_of_two_middles() {
        assert_eq!(median(&[10.0, 20.0]), Some(15.0));
        assert_eq!(median(&[10.0, 20.0, 30.0, 40.0]), Some(25.0));
    }

    #[test]
    fn handles_negatives() {
        assert_eq!(median(&[-5.0, 0.0, 5.0]), Some(0.0));
    }

    #[test]
    fn duplicate_values_handled() {
        // Three venues all reporting the same value — median is that
        // value.
        assert_eq!(median(&[50.0, 50.0, 50.0]), Some(50.0));
    }

    #[test]
    fn nan_sorts_last_with_total_cmp() {
        // Per total_cmp, NaN sorts greater than every non-NaN.
        // With one NaN in three values the middle is still the
        // largest finite value — defined behaviour, even if the
        // caller should have filtered it out upstream.
        let m = median(&[10.0, 20.0, f64::NAN]).unwrap();
        assert_eq!(m, 20.0);
    }

    #[test]
    fn nan_in_even_count_propagates_through_midpoint() {
        // Even-count branch averages the two middles via
        // `f64::midpoint`. With NaN as one of those middles the
        // result is NaN — caller-of-caller (the engine's `bvol`
        // gate) drops non-finite inputs before they reach this
        // function in production, but the propagation is the
        // defined behaviour to assert against.
        let m = median(&[10.0, f64::NAN]).unwrap();
        assert!(m.is_nan(), "even-count NaN must propagate; got {m}");
    }
}
