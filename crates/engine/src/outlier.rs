//! Per-venue outlier drop policy (issue #63, `math_reference.md:71`).
//!
//! Median blend (#61) is robust against *one* tick of bad data, but
//! does not exclude a *persistently* misbehaving venue from the input
//! set. A venue stuck on a frozen quote keeps skewing the median
//! window after window — this module is the active-defense layer
//! that drops it after a streak of consecutive deviations.
//!
//! Policy: **drop venue if its per-tick BVOL deviates > `threshold_pct`
//! from the current cross-venue median for `streak_required`+
//! consecutive ticks.** A single tick back in band resets the streak —
//! transient ratelimit hiccups or brief gaps do not get a venue
//! kicked out.
//!
//! Defaults (per `math_reference.md:71`): `threshold_pct = 0.05` (5 %),
//! `streak_required = 5` ticks. Both overridable via env
//! (`ENGINE_OUTLIER_THRESHOLD_PCT`, `ENGINE_OUTLIER_STREAK`) for the
//! 30 d DVOL benchmark match window.
//!
//! Pure logic — no I/O, no metrics, no logging. Inputs in, deltas
//! out; the call site (`snapshot::run_snapshot`) handles the
//! side-effects so this module stays unit-testable against synthetic
//! tick streams.

use std::collections::{HashMap, HashSet};

use volx_shared_types::ids::Venue;

use crate::blend;

/// Default deviation threshold (5 % per `math_reference.md:71`).
pub const DEFAULT_THRESHOLD_PCT: f64 = 0.05;

/// Default streak length before drop (5 ticks per `math_reference.md:71`).
pub const DEFAULT_STREAK_REQUIRED: u32 = 5;

/// One venue's per-tick contribution as seen by the tracker.
#[derive(Debug, Clone, Copy)]
pub struct VenueValue {
    pub venue: Venue,
    pub value: f64,
}

/// Outcome of one [`OutlierTracker::evaluate`] call.
///
/// `active` is the venue subset the median blend should consume on
/// this tick (venues whose streak has not yet hit the drop
/// threshold). `dropped` / `restored` are the per-tick deltas the
/// scheduler logs + emits metrics for.
///
/// `active` is a [`HashSet`] for O(1) `contains` at the partition
/// site in `snapshot::run_snapshot`. Iteration order is undefined —
/// callers that need a stable order must sort by [`Venue::label`]
/// (the same convention `run_snapshot` already uses for the
/// per-venue iteration).
#[derive(Debug, Default, Clone)]
pub struct EvalDelta {
    pub active: HashSet<Venue>,
    /// Venues that hit the streak threshold **on this tick** for the
    /// first time. Already-dropped venues that stay dropped do NOT
    /// appear here — the scheduler should log only the transition.
    pub newly_dropped: Vec<DroppedVenue>,
    /// Venues that were dropped on a prior tick and have returned
    /// to within `threshold_pct` of the median this tick.
    pub newly_restored: Vec<Venue>,
    /// Current median over the input slice (before any drops were
    /// applied). Surfaced for the drop log line — operators want to
    /// see "venue X at 65.2, median 50.1, deviation 30 %" not just
    /// "venue X dropped".
    pub median: f64,
}

/// Per-venue context for a drop log line — the scheduler does the
/// actual `info!` call so this module stays pure.
#[derive(Debug, Clone, Copy)]
pub struct DroppedVenue {
    pub venue: Venue,
    pub value: f64,
    pub deviation_pct: f64,
    pub streak: u32,
}

/// Cross-tick state for the outlier policy. Owned by the scheduler
/// loop in the engine binary so `streaks` persists across the 60-second
/// snapshot cadence — that persistence is the whole point of the
/// 5-tick rule.
#[derive(Debug, Clone)]
pub struct OutlierTracker {
    /// Per-venue current streak length. A venue not in the map has
    /// never deviated (or its streak was just reset).
    streaks: HashMap<Venue, u32>,
    /// Set of venues currently dropped (streak ≥ `streak_required`).
    /// Tracked separately from `streaks` so the restore-transition
    /// log only fires once per re-inclusion.
    dropped: HashSet<Venue>,
    threshold_pct: f64,
    streak_required: u32,
}

impl OutlierTracker {
    /// Construct with the default thresholds from `math_reference.md:71`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_THRESHOLD_PCT, DEFAULT_STREAK_REQUIRED)
    }

    /// Construct with caller-supplied thresholds — used by the
    /// scheduler to wire the env-var overrides.
    #[must_use]
    pub fn with_thresholds(threshold_pct: f64, streak_required: u32) -> Self {
        Self {
            streaks: HashMap::new(),
            dropped: HashSet::new(),
            threshold_pct,
            streak_required,
        }
    }

    /// Evaluate the outlier policy for one tick.
    ///
    /// Returns the active set (venues to include in the blend), plus
    /// the per-tick transitions the scheduler should log + emit
    /// metrics for. The median in the result is the *raw* median
    /// over the input slice — the input the policy was evaluated
    /// against, not the post-drop blended value.
    ///
    /// Single-venue tick: never drops the only venue. That's an
    /// availability decision (publish degraded vs publish nothing)
    /// which lives in `snapshot::run_snapshot`'s `NoVenuesLive` path,
    /// not here.
    ///
    /// # Panics
    ///
    /// Internal invariant: after the `values.len() < 2` guard,
    /// `blend::median` on the non-empty slice cannot return `None`.
    /// An `expect()` enforces this — a panic here indicates a logic
    /// bug in the guard, never a data condition.
    pub fn evaluate(&mut self, values: &[VenueValue]) -> EvalDelta {
        // Empty / single-venue → pass-through. With 1 venue the
        // median is identity, every venue is within threshold of
        // itself, and the policy has nothing to do.
        if values.len() < 2 {
            return EvalDelta {
                active: values.iter().map(|v| v.venue).collect(),
                newly_dropped: Vec::new(),
                newly_restored: Vec::new(),
                median: values.first().map_or(f64::NAN, |v| v.value),
            };
        }

        let medians: Vec<f64> = values.iter().map(|v| v.value).collect();
        // `blend::median` cannot return `None` here — `values.len() >= 2`.
        let median = blend::median(&medians)
            .expect("values is non-empty (guarded above); median cannot return None");

        let mut newly_dropped = Vec::new();
        let mut newly_restored = Vec::new();
        let mut active = HashSet::new();

        // Edge case: median == 0 (or non-finite). Relative deviation
        // is undefined; skip evaluation this tick rather than divide
        // by zero. Every venue stays in its current state (no
        // streak change, no transitions). Cannot happen with real
        // BVOL values (always positive) but the engine should not
        // panic on a degenerate synthetic input.
        if !median.is_finite() || median.abs() < f64::EPSILON {
            for v in values {
                if !self.dropped.contains(&v.venue) {
                    active.insert(v.venue);
                }
            }
            return EvalDelta {
                active,
                newly_dropped,
                newly_restored,
                median,
            };
        }

        for v in values {
            let deviation_pct = ((v.value - median) / median).abs();
            let out_of_band = deviation_pct > self.threshold_pct;

            if out_of_band {
                let streak = self.streaks.entry(v.venue).or_insert(0);
                // Saturating: a venue stuck dropped for u32::MAX
                // ticks (≈ 136 y at 60 s cadence) must not wrap.
                // Wraparound would silently break the streak
                // comparison; saturate at the max instead.
                *streak = streak.saturating_add(1);
                let streak_now = *streak;

                if streak_now >= self.streak_required {
                    // Already dropped? Stay dropped silently. New
                    // drop? Record the transition.
                    if self.dropped.insert(v.venue) {
                        newly_dropped.push(DroppedVenue {
                            venue: v.venue,
                            value: v.value,
                            deviation_pct,
                            streak: streak_now,
                        });
                    }
                } else {
                    // Streak building but not yet at threshold —
                    // venue stays active.
                    active.insert(v.venue);
                }
            } else {
                // In band → reset streak.
                self.streaks.remove(&v.venue);
                // If this venue was previously dropped, restore it.
                if self.dropped.remove(&v.venue) {
                    newly_restored.push(v.venue);
                }
                active.insert(v.venue);
            }
        }

        // Availability guard: if the drops on this tick would leave
        // the active set empty (e.g. exactly 2 surviving venues with
        // symmetric deviation from the midpoint median — both look
        // equally out-of-band), roll back the new drops. The blend
        // must publish *something* over the active set; an empty
        // active set is an availability failure, distinct from a
        // quality failure. Streak counts are preserved so the next
        // tick re-evaluates with the same history.
        //
        // The newly-dropped venues are returned to the active set
        // and removed from `self.dropped`; `newly_dropped` is
        // cleared so the scheduler does not log a drop that did
        // not actually take effect.
        if active.is_empty() && !newly_dropped.is_empty() {
            for d in &newly_dropped {
                self.dropped.remove(&d.venue);
                active.insert(d.venue);
            }
            newly_dropped.clear();
        }

        EvalDelta {
            active,
            newly_dropped,
            newly_restored,
            median,
        }
    }

    /// Current count of dropped venues. Used by the
    /// `volx_engine_active_venues` gauge in the scheduler loop.
    #[must_use]
    pub fn dropped_count(&self) -> usize {
        self.dropped.len()
    }
}

impl Default for OutlierTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn vv(venue: Venue, value: f64) -> VenueValue {
        VenueValue { venue, value }
    }

    /// Synthetic 3-venue tick: median venue + drift venue + base venue.
    /// Returns a `Vec` so caller can mutate per-tick.
    fn three(b: f64, d: f64, o: f64) -> Vec<VenueValue> {
        vec![
            vv(Venue::Bybit, b),
            vv(Venue::Deribit, d),
            vv(Venue::Okx, o),
        ]
    }

    #[test]
    fn empty_input_yields_empty_active() {
        let mut t = OutlierTracker::new();
        let d = t.evaluate(&[]);
        assert!(d.active.is_empty());
        assert!(d.newly_dropped.is_empty());
    }

    #[test]
    fn single_venue_never_dropped() {
        // Even with a venue value way off any plausible band, with
        // only one input there is no median to compare against —
        // pass-through, no streak, no drop.
        let mut t = OutlierTracker::new();
        for _ in 0..20 {
            let d = t.evaluate(&[vv(Venue::Deribit, 999.0)]);
            assert_eq!(d.active.len(), 1);
            assert!(d.newly_dropped.is_empty());
            assert!(d.newly_restored.is_empty());
        }
    }

    #[test]
    fn drop_fires_exactly_at_streak_threshold_not_before() {
        // Three venues: Bybit + Deribit at 50.0 (median), Okx
        // drifts at 53.0 (+6 % > 5 % threshold). With the default
        // streak_required = 5, Okx should be dropped on tick 5,
        // not tick 4.
        let mut t = OutlierTracker::new();
        for tick in 1..=5 {
            let d = t.evaluate(&three(50.0, 50.0, 53.0));
            if tick < 5 {
                assert!(
                    d.newly_dropped.is_empty(),
                    "tick {tick}: should not drop yet"
                );
                assert_eq!(d.active.len(), 3, "tick {tick}: all venues active");
            } else {
                assert_eq!(d.newly_dropped.len(), 1);
                assert_eq!(d.newly_dropped[0].venue, Venue::Okx);
                assert_eq!(d.newly_dropped[0].streak, 5);
                assert!((d.newly_dropped[0].deviation_pct - 0.06).abs() < 1e-12);
                assert_eq!(d.active.len(), 2, "Okx is out of active set");
                assert!(!d.active.contains(&Venue::Okx));
            }
        }
    }

    #[test]
    fn drop_does_not_re_log_after_initial_transition() {
        let mut t = OutlierTracker::new();
        for _ in 0..5 {
            t.evaluate(&three(50.0, 50.0, 53.0));
        }
        // Tick 6: Okx still out of band.
        let d = t.evaluate(&three(50.0, 50.0, 53.0));
        assert!(
            d.newly_dropped.is_empty(),
            "already-dropped venue should not re-log"
        );
        assert!(!d.active.contains(&Venue::Okx));
    }

    #[test]
    fn streak_resets_when_venue_returns_in_band() {
        // Drift for 4 ticks (one below threshold of 5) then back in
        // band → never dropped.
        let mut t = OutlierTracker::new();
        for _ in 0..4 {
            let d = t.evaluate(&three(50.0, 50.0, 55.0)); // +10 % drift
            assert!(d.newly_dropped.is_empty());
        }
        // Tick 5: back to 50.0. Streak resets, no drop ever fired.
        let d = t.evaluate(&three(50.0, 50.0, 50.0));
        assert!(d.newly_dropped.is_empty());
        assert!(d.newly_restored.is_empty(), "never dropped → no restore");
        assert_eq!(d.active.len(), 3);
        // Tick 6: another 4-tick drift — must start fresh, not
        // build on the prior streak.
        for _ in 0..4 {
            let d = t.evaluate(&three(50.0, 50.0, 55.0));
            assert!(d.newly_dropped.is_empty());
        }
    }

    #[test]
    fn restoration_fires_once_when_venue_returns_after_drop() {
        let mut t = OutlierTracker::new();
        // 5 ticks of drift → drop on tick 5.
        for _ in 0..5 {
            t.evaluate(&three(50.0, 50.0, 53.0));
        }
        // Tick 6: Okx returns to median.
        let d = t.evaluate(&three(50.0, 50.0, 50.0));
        assert_eq!(d.newly_restored, vec![Venue::Okx]);
        assert!(d.active.contains(&Venue::Okx));
        assert_eq!(d.active.len(), 3);
        // Tick 7: still in band — must not re-log the restore.
        let d = t.evaluate(&three(50.0, 50.0, 50.0));
        assert!(d.newly_restored.is_empty());
    }

    #[test]
    fn two_opposite_drifts_drop_both_after_streak() {
        // Bybit at 50, Deribit at +6 %, Okx at -6 %. Median is 50
        // (the un-drifted venue). Both drifters hit the streak at
        // tick 5 → drop both, degrade to 1 venue.
        let mut t = OutlierTracker::new();
        for tick in 1..=5 {
            let d = t.evaluate(&three(50.0, 53.0, 47.0));
            if tick < 5 {
                assert_eq!(d.active.len(), 3);
            } else {
                assert_eq!(d.newly_dropped.len(), 2);
                let dropped: HashSet<Venue> = d.newly_dropped.iter().map(|x| x.venue).collect();
                assert!(dropped.contains(&Venue::Deribit));
                assert!(dropped.contains(&Venue::Okx));
                assert_eq!(d.active.len(), 1);
                assert!(d.active.contains(&Venue::Bybit));
            }
        }
    }

    #[test]
    fn just_under_threshold_never_drops() {
        // 4.9 % drift < 5 % threshold → no streak ever builds.
        let mut t = OutlierTracker::new();
        for _ in 0..50 {
            let d = t.evaluate(&three(50.0, 50.0, 50.0 * 1.049));
            assert!(d.newly_dropped.is_empty());
            assert_eq!(d.active.len(), 3);
        }
    }

    #[test]
    fn zero_median_degenerate_input_does_not_panic() {
        // Cannot happen with real BVOL but the policy must not
        // divide by zero on a synthetic / corrupted input.
        let mut t = OutlierTracker::new();
        let d = t.evaluate(&three(0.0, 0.0, 0.0));
        assert_eq!(d.active.len(), 3, "degenerate tick stays in-band");
        assert!(d.newly_dropped.is_empty());
    }

    #[test]
    fn two_venues_both_out_of_band_rolls_back_drops_for_availability() {
        // Exactly 2 venues, symmetric divergence at the streak
        // threshold: both deviate +/-3 % from the midpoint median.
        // With threshold=2 %, streak=1 both would drop on tick 1,
        // leaving the active set empty. The availability guard
        // rolls back the drops — blend must always have something
        // to publish. Streak counts stay so a future asymmetric
        // tick can still drop one venue.
        let mut t = OutlierTracker::with_thresholds(0.02, 1);
        let d = t.evaluate(&[vv(Venue::Deribit, 51.5), vv(Venue::Okx, 48.5)]);
        // Both venues survive — rolled back.
        assert_eq!(d.active.len(), 2);
        assert!(d.newly_dropped.is_empty(), "drops rolled back");
        // `dropped` set must be empty after rollback so the
        // restore-transition log doesn't spuriously fire next tick.
        assert_eq!(t.dropped_count(), 0);
    }

    #[test]
    fn custom_thresholds_take_effect() {
        // 1 % threshold, 2-tick streak. A 1.5 % drift drops on
        // tick 2.
        let mut t = OutlierTracker::with_thresholds(0.01, 2);
        let d = t.evaluate(&three(50.0, 50.0, 50.75));
        assert!(d.newly_dropped.is_empty());
        let d = t.evaluate(&three(50.0, 50.0, 50.75));
        assert_eq!(d.newly_dropped.len(), 1);
        assert_eq!(d.newly_dropped[0].venue, Venue::Okx);
        assert_eq!(d.newly_dropped[0].streak, 2);
    }

    #[test]
    fn dropped_count_tracks_current_drops() {
        let mut t = OutlierTracker::new();
        assert_eq!(t.dropped_count(), 0);
        for _ in 0..5 {
            t.evaluate(&three(50.0, 50.0, 53.0));
        }
        assert_eq!(t.dropped_count(), 1);
        // Restore.
        t.evaluate(&three(50.0, 50.0, 50.0));
        assert_eq!(t.dropped_count(), 0);
    }
}
