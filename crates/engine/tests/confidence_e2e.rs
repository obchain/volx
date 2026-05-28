//! End-to-end confidence — exercises the full `run_snapshot` path
//! and asserts the published `IndexValue.confidence` matches the
//! `METHODOLOGY.md` §5.1 formula. Fixtures use synthetic flat-IV
//! chains so the per-venue BVOL is predictable; this file focuses
//! on the confidence dimension specifically (venues × freshness ×
//! strikes), with median-blend behaviour locked in `median_blend.rs`.

#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use time::OffsetDateTime;
use volx_shared_types::Asset;
use volx_shared_types::ids::{IndexId, Venue};
use volx_shared_types::units::Years;

use volx_engine::chain::{AssetChains, MultiVenueChains, VenueChains};
use volx_engine::confidence::METHODOLOGY_MIN_STRIKES;
use volx_engine::outlier::OutlierTracker;
use volx_engine::strip::{ChainLeg, ExpiryChain};
use volx_engine::{SnapshotError, bs};

const NEAR_T: f64 = 14.0 / 365.0;
const NEXT_T: f64 = 60.0 / 365.0;

fn now() -> OffsetDateTime {
    time::macros::datetime!(2026-05-25 12:00:00 UTC)
}

/// Flat-IV chain with at least `METHODOLOGY_MIN_STRIKES` listed strikes
/// so the `strike_term` lands at 1.0 by default.
fn flat_iv_chain(t: f64, iv: f64) -> ExpiryChain {
    let forward = 100.0;
    let step = 4.0;
    let n_pairs: i64 = 30;
    let mut legs = Vec::new();
    let half = n_pairs;
    for i in -half..=half {
        #[allow(clippy::cast_precision_loss)]
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

/// Build one venue with a near + next expiry pair and an explicit
/// `latest_ts` (so tests dial freshness directly without monkey-
/// patching the system clock).
fn venue_with_freshness(latest_ts: OffsetDateTime, iv: f64) -> VenueChains {
    let mut per_venue = VenueChains {
        assets: AssetChains::default(),
        latest_ts,
    };
    let per_asset = per_venue.assets.entry(Asset::Btc).or_default();
    let near_expiry = time::macros::datetime!(2026-06-01 08:00:00 UTC);
    let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
    per_asset.insert(near_expiry, flat_iv_chain(NEAR_T, iv));
    per_asset.insert(next_expiry, flat_iv_chain(NEXT_T, iv));
    per_venue
}

/// Slice the centre N legs out of the full flat-IV chain so the
/// forward picker (which scans for the strike minimising |C - P|)
/// still finds a two-sided strike straddling the underlying. Drops
/// from the wings, not the middle.
fn centred_chain(t: f64, iv: f64, keep: usize) -> ExpiryChain {
    let full = flat_iv_chain(t, iv);
    let n = full.legs.len();
    let half_keep = keep / 2;
    let mid = n / 2;
    let lo = mid.saturating_sub(half_keep);
    let hi = (mid + keep.div_ceil(2)).min(n);
    ExpiryChain {
        time_to_expiry: full.time_to_expiry,
        legs: full.legs[lo..hi].to_vec(),
    }
}

/// Same shape as [`venue_with_freshness`] but the user picks the
/// listed-strike count by clipping the chain. Used to drive the
/// `strike_term` toward < 1.0 without changing the venue count or
/// the freshness.
fn venue_with_listed_count(latest_ts: OffsetDateTime, iv: f64, keep: usize) -> VenueChains {
    let mut per_venue = VenueChains {
        assets: AssetChains::default(),
        latest_ts,
    };
    let per_asset = per_venue.assets.entry(Asset::Btc).or_default();
    per_asset.insert(
        time::macros::datetime!(2026-06-01 08:00:00 UTC),
        centred_chain(NEAR_T, iv, keep),
    );
    per_asset.insert(
        time::macros::datetime!(2026-07-01 08:00:00 UTC),
        centred_chain(NEXT_T, iv, keep),
    );
    per_venue
}

#[test]
fn three_venues_all_fresh_full_strikes_yields_one() {
    // venue_term = 3/3 = 1, fresh_term = 1 - 0/60 = 1,
    // strike_term = (>>8)/8 clamped to 1. Product = 1.0.
    let mut chains: MultiVenueChains = HashMap::new();
    for v in [Venue::Bybit, Venue::Deribit, Venue::Okx] {
        chains.insert(v, venue_with_freshness(now(), 0.5));
    }
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    assert_eq!(res.value.confidence, 1.0);
}

#[test]
fn two_of_three_venues_yields_two_thirds() {
    // venue_term = 2/3, fresh = 1, strike = 1 → ≈ 0.667.
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Deribit, venue_with_freshness(now(), 0.5));
    chains.insert(Venue::Okx, venue_with_freshness(now(), 0.5));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    assert!(
        (res.value.confidence - 2.0 / 3.0).abs() < 1e-12,
        "expected ≈ 0.667, got {}",
        res.value.confidence
    );
}

#[test]
fn single_venue_yields_one_third() {
    // venue_term = 1/3, fresh = 1, strike = 1 → ≈ 0.333.
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Deribit, venue_with_freshness(now(), 0.5));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    assert!(
        (res.value.confidence - 1.0 / 3.0).abs() < 1e-12,
        "expected ≈ 0.333, got {}",
        res.value.confidence
    );
}

#[test]
fn stale_quotes_demote_confidence_via_fresh_term() {
    // Oldest of the newests is `now - 30s` → fresh_term = 0.5.
    // venue_term = 1 (3/3), strike = 1, so confidence = 0.5.
    let stale = now() - time::Duration::seconds(30);
    let fresh = now();
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Bybit, venue_with_freshness(fresh, 0.5));
    chains.insert(Venue::Deribit, venue_with_freshness(stale, 0.5));
    chains.insert(Venue::Okx, venue_with_freshness(fresh, 0.5));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    assert!(
        (res.value.confidence - 0.5).abs() < 1e-12,
        "expected ≈ 0.5, got {}",
        res.value.confidence
    );
}

#[test]
fn quote_age_beyond_budget_collapses_confidence_to_zero() {
    // 90 s > 60 s FRESH_BUDGET_S → fresh_term clamps to 0,
    // confidence = 0. Snapshot still publishes — confidence is
    // a quality signal, not a publish gate.
    let very_stale = now() - time::Duration::seconds(90);
    let fresh = now();
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Deribit, venue_with_freshness(very_stale, 0.5));
    chains.insert(Venue::Okx, venue_with_freshness(fresh, 0.5));
    chains.insert(Venue::Bybit, venue_with_freshness(fresh, 0.5));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    assert_eq!(res.value.confidence, 0.0);
    // Sanity: the snapshot still published a value (confidence is
    // not a publish gate).
    assert!(res.value.value.is_finite());
}

#[test]
fn thin_strip_reduces_confidence_linearly() {
    // One venue with only 6 listed strikes (centred around the
    // underlying so the forward picker survives). 6 / 8 strikes =
    // 0.75 strike_term, venue_term = 1/3, fresh = 1. Product =
    // 1/3 * 0.75 = 0.25.
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Deribit, venue_with_listed_count(now(), 0.5, 6));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new())
            .unwrap();
    #[allow(clippy::cast_precision_loss)] // METHODOLOGY_MIN_STRIKES = 8, lossless in f64
    let expected = (1.0 / 3.0) * (6.0 / METHODOLOGY_MIN_STRIKES as f64);
    assert!(
        (res.value.confidence - expected).abs() < 1e-12,
        "expected ≈ {expected}, got {}",
        res.value.confidence
    );
}

#[test]
fn confidence_always_in_unit_interval() {
    // Adversarial mix: venue overprovisioned, strikes overprovisioned,
    // freshness perfect. All three terms saturate at 1.0 — confidence
    // must clamp to ≤ 1.0 not blow past it.
    let mut chains: MultiVenueChains = HashMap::new();
    for v in [Venue::Bybit, Venue::Deribit, Venue::Okx] {
        chains.insert(
            v,
            venue_with_listed_count(now(), 0.5, METHODOLOGY_MIN_STRIKES * 4),
        );
    }
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 1, &mut OutlierTracker::new())
            .unwrap();
    assert!((0.0..=1.0).contains(&res.value.confidence));
    assert_eq!(res.value.confidence, 1.0);
}

#[test]
fn venues_expected_env_dialed_down_during_rollout() {
    // Two venues live + venues_expected = 2 (operator dialed down
    // before Bybit shipped) → venue_term = 2/2 = 1 instead of 2/3,
    // so confidence stays at 1.0 instead of 0.667. The env var
    // path is the user-facing knob for this case.
    let mut chains: MultiVenueChains = HashMap::new();
    chains.insert(Venue::Deribit, venue_with_freshness(now(), 0.5));
    chains.insert(Venue::Okx, venue_with_freshness(now(), 0.5));
    let res =
        volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 2, &mut OutlierTracker::new())
            .unwrap();
    assert_eq!(res.value.confidence, 1.0);
}

#[test]
fn missing_asset_does_not_carry_confidence() {
    // No chain data → SnapshotError::MissingAsset, no IndexValue.
    // Confidence is a property of the published row; an unpublished
    // tick has no confidence to assert against.
    let chains: MultiVenueChains = HashMap::new();
    match volx_engine::run_snapshot(&chains, IndexId::Bvol, now(), 3, &mut OutlierTracker::new()) {
        Err(SnapshotError::MissingAsset(Asset::Btc)) => {}
        other => panic!("expected MissingAsset(Btc), got {other:?}"),
    }
}
