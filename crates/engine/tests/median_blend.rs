//! Cross-venue median blend — end-to-end through `run_snapshot`
//! (issue #61, `math_reference.md:71`).
//!
//! These tests exercise the full per-venue → median path against
//! synthetic flat-IV chains so the expected per-venue BVOL is
//! predictable to ~5 vol points (the wing-truncation tolerance).
//! Each test seeds 1, 2, or 3 venues with a different flat IV and
//! asserts the blended publish lands at the median of the per-venue
//! BVOLs — which, for flat-IV chains, is just `100 · iv` per venue.

// `float_cmp` is fine here: every comparison either uses a tolerance
// or compares values produced by identical inputs through identical
// code paths (e.g. single-venue passthrough — `median([x]) == x` bit-
// for-bit, no FP rounding).
#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use time::OffsetDateTime;
use volx_shared_types::Asset;
use volx_shared_types::ids::{IndexId, Venue};
use volx_shared_types::units::Years;

use volx_engine::chain::{AssetChains, MultiVenueChains};
use volx_engine::strip::{ChainLeg, ExpiryChain};
use volx_engine::{SnapshotError, bs};

const NEAR_T: f64 = 14.0 / 365.0;
const NEXT_T: f64 = 60.0 / 365.0;
/// Wing-truncation tolerance for flat-IV chains. Verified empirically
/// in `snapshot::tests::happy_path_produces_bvol_near_iv`.
const BVOL_TOL: f64 = 5.0;

fn now() -> OffsetDateTime {
    time::macros::datetime!(2026-05-25 12:00:00 UTC)
}

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

/// Build a single venue's [`AssetChains`] with one near + one next
/// expiry on BTC at the given flat IV.
fn venue_chains(iv: f64) -> AssetChains {
    let mut per_venue: AssetChains = HashMap::new();
    let per_asset = per_venue.entry(Asset::Btc).or_default();
    let near_expiry = time::macros::datetime!(2026-06-01 08:00:00 UTC);
    let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
    per_asset.insert(near_expiry, flat_iv_chain(100.0, 4.0, 30, NEAR_T, iv));
    per_asset.insert(next_expiry, flat_iv_chain(100.0, 4.0, 30, NEXT_T, iv));
    per_venue
}

fn multi_venue(venues: &[(Venue, f64)]) -> MultiVenueChains {
    let mut out: MultiVenueChains = HashMap::new();
    for &(v, iv) in venues {
        out.insert(v, venue_chains(iv));
    }
    out
}

#[test]
fn single_venue_publishes_passthrough() {
    // Degraded mode: only Deribit is live. `median([x]) == x`, so
    // the blended publish must equal the single venue's BVOL —
    // identical to the pre-#61 single-venue behaviour.
    let chains = multi_venue(&[(Venue::Deribit, 0.5)]);
    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue.len(), 1);
    assert_eq!(res.per_venue[0].venue, Venue::Deribit);
    // Blend == per-venue value (exactly — single-element median is
    // identity, no rounding).
    assert_eq!(res.value.value, res.per_venue[0].value);
    // Sanity-check it's in the expected ballpark for iv=0.5.
    assert!(
        (res.value.value - 50.0).abs() < BVOL_TOL,
        "BVOL={} expected ≈ 50",
        res.value.value
    );
}

#[test]
fn three_venues_blend_to_median() {
    // Three venues at iv = 0.4 / 0.5 / 0.6. Per-venue BVOLs ≈
    // 40 / 50 / 60. Median = 50 (the middle Deribit value).
    let chains = multi_venue(&[
        (Venue::Bybit, 0.4),
        (Venue::Deribit, 0.5),
        (Venue::Okx, 0.6),
    ]);
    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue.len(), 3);
    // Iteration order is alpha on Venue.label(): bybit < deribit < okx.
    assert_eq!(res.per_venue[0].venue, Venue::Bybit);
    assert_eq!(res.per_venue[1].venue, Venue::Deribit);
    assert_eq!(res.per_venue[2].venue, Venue::Okx);
    // Blend == middle per-venue value.
    let middle = res.per_venue[1].value;
    assert!(
        (res.value.value - middle).abs() < 1e-12,
        "blend {} should equal middle per-venue {}",
        res.value.value,
        middle
    );
}

#[test]
fn three_venues_one_outlier_median_ignores_it() {
    // Two venues at iv = 0.5 (BVOL ≈ 50), one rogue venue at iv = 5.0
    // (BVOL ≈ 500 — fat-finger / stuck-feed simulation). Median of
    // three is the middle value, so the blend lands at the
    // non-rogue level — the rogue venue cannot drag the index.
    let chains = multi_venue(&[
        (Venue::Bybit, 0.5),
        (Venue::Deribit, 0.5),
        (Venue::Okx, 5.0),
    ]);
    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue.len(), 3);
    // The Okx per-venue value is the outlier; the median must be
    // one of the two non-Okx values.
    let okx_bvol = res
        .per_venue
        .iter()
        .find(|v| v.venue == Venue::Okx)
        .unwrap()
        .value;
    assert!(
        okx_bvol > 100.0,
        "Okx per-venue should be the outlier (iv=5.0 → BVOL ≫ 100); got {okx_bvol}"
    );
    assert!(
        (res.value.value - 50.0).abs() < BVOL_TOL,
        "blend should reject the outlier and land near 50; got {}",
        res.value.value
    );
}

#[test]
fn two_venues_blend_is_mean_of_pair() {
    // Even-count median = arithmetic mean of the two middle values.
    // The two venues *are* the two middle values, so the blend is
    // the per-venue arithmetic mean. This is the documented even-
    // count behaviour — outlier protection requires three venues.
    let chains = multi_venue(&[(Venue::Deribit, 0.4), (Venue::Okx, 0.6)]);
    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue.len(), 2);
    let expected = f64::midpoint(res.per_venue[0].value, res.per_venue[1].value);
    assert!(
        (res.value.value - expected).abs() < 1e-12,
        "two-venue blend {} should equal arithmetic mean {}",
        res.value.value,
        expected
    );
}

#[test]
fn no_venues_live_rejects_with_dedicated_error() {
    // Every present venue fails its per-venue pipeline (here:
    // only a next expiry exists, so each venue trips
    // `NoNearExpiry`). The blend has zero survivors and
    // `run_snapshot` returns `NoVenuesLive` — distinct from
    // `MissingAsset`, which fires only when no venue has any data
    // for the asset at all.
    let now = now();
    let mut chains: MultiVenueChains = HashMap::new();
    for venue in [Venue::Deribit, Venue::Okx] {
        let mut per_venue: AssetChains = HashMap::new();
        let per_asset = per_venue.entry(Asset::Btc).or_default();
        per_asset.insert(
            time::macros::datetime!(2026-07-01 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, NEXT_T, 0.5),
        );
        chains.insert(venue, per_venue);
    }
    match volx_engine::run_snapshot(&chains, IndexId::Bvol, now) {
        Err(SnapshotError::NoVenuesLive {
            asset: Asset::Btc,
            per_venue_errors,
        }) => {
            // Both venues failed — per-venue breakdown lets the
            // scheduler fire the per-venue counter on this total-
            // failure path, distinguishing roll-date events from
            // data-quality regressions across the fleet.
            assert_eq!(per_venue_errors.len(), 2);
            for (_, e) in &per_venue_errors {
                assert_eq!(e.as_label(), "no_near_expiry");
            }
        }
        other => panic!("expected NoVenuesLive(Btc), got {other:?}"),
    }
}

#[test]
fn missing_asset_distinguished_from_no_venues_live() {
    // No venue has any chain data for the asset at all — distinct
    // operational state from "venues had data but every per-venue
    // pipeline failed" (NoVenuesLive). The two error labels back
    // separate dashboards.
    let chains: MultiVenueChains = HashMap::new();
    match volx_engine::run_snapshot(&chains, IndexId::Bvol, now()) {
        Err(SnapshotError::MissingAsset(Asset::Btc)) => {}
        other => panic!("expected MissingAsset(Btc), got {other:?}"),
    }
}

#[test]
fn partial_venue_failure_still_publishes_with_survivors() {
    // Two venues live, one venue has only-next-expiry chain (its
    // per-venue pipeline trips NoNearExpiry). The blend uses the
    // two live venues and `per_venue_errors` records the failed
    // one — operators see the venue dropping out in the per-venue
    // counter without losing the published index for the tick.
    let mut chains = multi_venue(&[(Venue::Deribit, 0.5), (Venue::Okx, 0.5)]);
    let mut bybit_chains: AssetChains = HashMap::new();
    let per_asset = bybit_chains.entry(Asset::Btc).or_default();
    per_asset.insert(
        time::macros::datetime!(2026-07-01 08:00:00 UTC),
        flat_iv_chain(100.0, 4.0, 30, NEXT_T, 0.5),
    );
    chains.insert(Venue::Bybit, bybit_chains);

    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue.len(), 2);
    assert_eq!(res.per_venue_errors.len(), 1);
    assert_eq!(res.per_venue_errors[0].0, Venue::Bybit);
    // Blend still lands near 50 because the two live venues both
    // report iv=0.5.
    assert!(
        (res.value.value - 50.0).abs() < BVOL_TOL,
        "blend with two healthy venues should land near 50; got {}",
        res.value.value
    );
}

#[test]
fn strip_hash_changes_when_venue_set_changes() {
    // Same per-venue input but a different venue mix must produce
    // a different audit hash — the hash folds Venue.label() so a
    // verifier can distinguish "same blend value, different venue
    // composition" (e.g. degraded-mode publish vs full publish).
    let two = multi_venue(&[(Venue::Deribit, 0.5), (Venue::Okx, 0.5)]);
    let three = multi_venue(&[
        (Venue::Bybit, 0.5),
        (Venue::Deribit, 0.5),
        (Venue::Okx, 0.5),
    ]);
    let a = volx_engine::run_snapshot(&two, IndexId::Bvol, now()).unwrap();
    let b = volx_engine::run_snapshot(&three, IndexId::Bvol, now()).unwrap();
    assert_ne!(a.value.strip_hash, b.value.strip_hash);
}

#[test]
fn primary_strips_is_alpha_first_venue() {
    // primary_strips() backs the existing single-strip
    // `last_strip` fanout — it must be deterministic. Alpha-first
    // venue: bybit < deribit < okx.
    let chains = multi_venue(&[
        (Venue::Bybit, 0.5),
        (Venue::Deribit, 0.5),
        (Venue::Okx, 0.5),
    ]);
    let res = volx_engine::run_snapshot(&chains, IndexId::Bvol, now()).unwrap();
    assert_eq!(res.per_venue[0].venue, Venue::Bybit);
    let (near, _next) = res.primary_strips().unwrap();
    // Strip itself is the Bybit venue's near-expiry strip — same
    // bytes for two identical floats, so exact equality is the
    // right check (no FP arithmetic involved).
    assert!((near.forward - res.per_venue[0].near.forward).abs() < f64::EPSILON);
    assert!((near.k_zero - res.per_venue[0].near.k_zero).abs() < f64::EPSILON);
}
