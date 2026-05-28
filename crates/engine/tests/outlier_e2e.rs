//! End-to-end outlier drop policy through `run_snapshot` — exercises
//! the cross-tick streak state by calling `run_snapshot` 5+ times with
//! deliberately drifted per-venue IV inputs, then asserting on which
//! venues land in `per_venue` vs `per_venue_dropped` and on the
//! `outlier_delta` transitions (issue #63, `math_reference.md:71`).
//!
//! These tests focus on the policy wiring across snapshot ticks. The
//! pure outlier logic (streak math, threshold edges) is unit-tested in
//! `crates/engine/src/outlier.rs`.

#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use time::OffsetDateTime;
use volx_shared_types::Asset;
use volx_shared_types::ids::{IndexId, Venue};
use volx_shared_types::units::Years;

use volx_engine::chain::{AssetChains, MultiVenueChains, VenueChains};
use volx_engine::outlier::OutlierTracker;
use volx_engine::strip::{ChainLeg, ExpiryChain};
use volx_engine::{SnapshotResult, bs};

const NEAR_T: f64 = 14.0 / 365.0;
const NEXT_T: f64 = 60.0 / 365.0;

fn now() -> OffsetDateTime {
    time::macros::datetime!(2026-05-25 12:00:00 UTC)
}

fn flat_iv_chain(t: f64, iv: f64) -> ExpiryChain {
    let forward = 100.0;
    let step = 4.0;
    let n_pairs: i64 = 30;
    let mut legs = Vec::new();
    for i in -n_pairs..=n_pairs {
        #[allow(clippy::cast_precision_loss)] // n_pairs ≤ 30, lossless in f64
        let k = forward + step * i as f64;
        if k <= 0.0 {
            continue;
        }
        let c = bs::call_price(forward, k, t, 0.0, iv);
        let p = bs::put_price(forward, k, t, 0.0, iv);
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

fn venue_chains(iv: f64) -> VenueChains {
    let mut per_venue = VenueChains {
        assets: AssetChains::default(),
        latest_ts: time::macros::datetime!(2026-05-25 11:59:59 UTC),
    };
    let per_asset = per_venue.assets.entry(Asset::Btc).or_default();
    per_asset.insert(
        time::macros::datetime!(2026-06-01 08:00:00 UTC),
        flat_iv_chain(NEAR_T, iv),
    );
    per_asset.insert(
        time::macros::datetime!(2026-07-01 08:00:00 UTC),
        flat_iv_chain(NEXT_T, iv),
    );
    per_venue
}

fn multi_venue(venues: &[(Venue, f64)]) -> MultiVenueChains {
    let mut out: MultiVenueChains = HashMap::new();
    for &(v, iv) in venues {
        out.insert(v, venue_chains(iv));
    }
    out
}

fn run_one(chains: &MultiVenueChains, tracker: &mut OutlierTracker) -> SnapshotResult {
    volx_engine::run_snapshot(chains, IndexId::Bvol, now(), 3, tracker).unwrap()
}

/// 6 % drift (above 5 % threshold) for 5 ticks → Okx dropped on tick 5.
/// Ticks 1-4 keep all three venues active. The synthetic IVs are
/// chosen so per-venue BVOL ≈ 100 × iv, so deviations track IV deltas
/// directly to ~5 vol-points tolerance (well outside the 5 % band).
#[test]
fn three_venues_one_drifts_six_percent_dropped_at_tick_five() {
    let mut t = OutlierTracker::new();
    let chains = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.53), // +6 % vs the 0.50 base → BVOL deviation ≈ 6 %
    ]);

    for tick in 1..=4 {
        let res = run_one(&chains, &mut t);
        assert_eq!(res.per_venue.len(), 3, "tick {tick}: all venues active");
        assert!(
            res.per_venue_dropped.is_empty(),
            "tick {tick}: no drops yet"
        );
        assert!(
            res.outlier_delta.newly_dropped.is_empty(),
            "tick {tick}: no drop transition"
        );
    }

    let res = run_one(&chains, &mut t);
    assert_eq!(res.outlier_delta.newly_dropped.len(), 1, "tick 5: 1 drop");
    assert_eq!(res.outlier_delta.newly_dropped[0].venue, Venue::Okx);
    assert_eq!(res.outlier_delta.newly_dropped[0].streak, 5);
    assert_eq!(res.per_venue.len(), 2, "active set degrades to 2");
    assert_eq!(res.per_venue_dropped.len(), 1);
    assert_eq!(res.per_venue_dropped[0].venue, Venue::Okx);
}

/// 10 % drift for 4 ticks → no drop (streak < 5); tick 5 in-band
/// resets the streak. Venue never dropped, no restore log fires.
#[test]
fn drift_for_four_ticks_then_return_never_drops() {
    let mut t = OutlierTracker::new();
    let drifted = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.55), // +10 % vs base
    ]);
    let in_band = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.50),
    ]);

    for tick in 1..=4 {
        let res = run_one(&drifted, &mut t);
        assert!(
            res.outlier_delta.newly_dropped.is_empty(),
            "tick {tick}: streak {tick} < 5, no drop"
        );
        assert_eq!(res.per_venue.len(), 3);
    }

    let res = run_one(&in_band, &mut t);
    assert!(
        res.outlier_delta.newly_dropped.is_empty(),
        "in-band tick must reset streak before any drop"
    );
    assert!(
        res.outlier_delta.newly_restored.is_empty(),
        "no prior drop → no restore"
    );
    assert_eq!(res.per_venue.len(), 3);
}

/// Two venues drift opposite directions (+6 % and -6 %). Median tracks
/// the un-drifted Bybit. After 5 ticks both Deribit and Okx are dropped
/// — active set degrades to 1 venue.
#[test]
fn opposite_drifts_drop_both_active_set_degrades_to_one() {
    let mut t = OutlierTracker::new();
    let chains = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.53), // +6 %
        (Venue::Okx, 0.47),     // -6 %
    ]);

    for _ in 1..=4 {
        let res = run_one(&chains, &mut t);
        assert_eq!(res.per_venue.len(), 3);
    }
    let res = run_one(&chains, &mut t);
    assert_eq!(res.outlier_delta.newly_dropped.len(), 2);
    assert_eq!(res.per_venue.len(), 1);
    assert_eq!(res.per_venue[0].venue, Venue::Bybit);
}

/// Single venue → outlier policy is pass-through. Even with an
/// absurd value the single-venue tick stays in the active set —
/// dropping the only venue would be an availability decision the
/// `NoVenuesLive` path owns, not a quality decision.
#[test]
fn single_venue_never_dropped_by_outlier_policy() {
    let mut t = OutlierTracker::new();
    let chains = multi_venue(&[(Venue::Deribit, 0.50)]);
    for _ in 0..10 {
        let res = run_one(&chains, &mut t);
        assert_eq!(res.per_venue.len(), 1);
        assert!(res.per_venue_dropped.is_empty());
        assert!(res.outlier_delta.newly_dropped.is_empty());
    }
}

/// After drop, the next in-band tick restores the venue. The
/// scheduler logs the restore exactly once.
#[test]
fn drop_then_restore_fires_restoration_transition() {
    let mut t = OutlierTracker::new();
    let drifted = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.53), // +6 %
    ]);
    let in_band = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.50),
    ]);

    // Drop at tick 5.
    for _ in 1..=5 {
        run_one(&drifted, &mut t);
    }
    // Tick 6: in-band → restore.
    let res = run_one(&in_band, &mut t);
    assert_eq!(res.outlier_delta.newly_restored, vec![Venue::Okx]);
    assert_eq!(res.per_venue.len(), 3);
    // Tick 7: still in-band — no re-log.
    let res = run_one(&in_band, &mut t);
    assert!(res.outlier_delta.newly_restored.is_empty());
}

/// Confidence reads the **active** venue count, not the live count.
/// One venue dropped by the outlier policy demotes confidence the
/// same way one venue failing the per-venue pipeline does. This is
/// the wire-contract for issue #62 + #63 composition.
#[test]
fn dropped_venue_demotes_confidence_score() {
    let mut t = OutlierTracker::new();
    let chains = multi_venue(&[
        (Venue::Bybit, 0.50),
        (Venue::Deribit, 0.50),
        (Venue::Okx, 0.53),
    ]);

    // Pre-drop: 3/3 venues × ~0.98 fresh (1s age) × 1.0 strike.
    // Capture the pre-drop value as the baseline so the post-drop
    // comparison isolates the venue-count change.
    let pre = run_one(&chains, &mut t);
    let pre_fresh = pre.value.confidence; // ≈ 3/3 × fresh × 1 = fresh
    assert!(
        pre_fresh > 0.95,
        "pre-drop confidence should be near 1; got {pre_fresh}"
    );

    // Build the streak; tick 5 drops Okx and pulls active set to 2.
    for _ in 2..=5 {
        run_one(&chains, &mut t);
    }
    let post = run_one(&chains, &mut t);
    // Post-drop venue_term collapses from 3/3 to 2/3 — confidence
    // should land at (2/3) × pre_fresh (same fresh_term + strike_term).
    let expected = (2.0 / 3.0) * pre_fresh;
    assert!(
        (post.value.confidence - expected).abs() < 1e-12,
        "confidence after drop should be 2/3 of pre-drop; expected {expected}, got {}",
        post.value.confidence
    );
}
