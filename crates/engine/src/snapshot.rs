//! Snapshot orchestrator — takes per-asset chains and produces one
//! [`IndexValue`] per index (BVOL / EVOL) per 60-second tick (issue #20).
//!
//! Pipeline per asset:
//!
//! 1. **Expiry picker (§4.1).** Near = largest listed expiry with
//!    `t_t ∈ [7d, 30d)`. Next = smallest listed expiry with `t_t > 30d`.
//!    If either is missing, the index is **not published** for that
//!    snapshot.
//! 2. **Strip builder (§4.2 + §4.3 + §4.5).** Run [`crate::build_strip`]
//!    on each of the two chains.
//! 3. **Variance integral (§4.5).** [`crate::variance_t`] yields
//!    annualised `σ²_T` for each expiry.
//! 4. **30-day interpolation (§4.6) + BVOL conversion (§4.7).**
//!    [`crate::interpolate_30d`] then [`crate::bvol`].
//!
//! On any rejection the snapshot is skipped (no row published) and a
//! `volx_engine_snapshot_rejected_total{index_id,reason}` counter
//! increments. The `index_ticks` schema does not (yet) have a status
//! column — `METHODOLOGY.md` §5 calls for a "null row with status"
//! shape that requires a schema migration; this PR defers that to a
//! future schema bump and uses the metric as the visibility signal.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::debug;
use volx_shared_types::ids::IndexId;
use volx_shared_types::index::{IndexValue, StripHash};
use volx_shared_types::strip::Strip;

use crate::chain::AssetChains;
use crate::interpolate::{ExpiryVariance, bvol, interpolate_30d};
use crate::strip::{BuildError, ExpiryChain, build_strip};
use crate::variance::{VarianceError, variance_t};

/// Time-to-expiry bounds (years). Hard-pinned from `METHODOLOGY.md`
/// §4.1; the §4.1 picker uses them to bracket 30 days.
const T_NEAR_MIN: f64 = 7.0 / 365.0;
const T_BRACKET: f64 = 30.0 / 365.0;

/// Why a per-index snapshot can fail. All variants are non-fatal at the
/// scheduler level — the per-tick driver logs + counter-emits and
/// continues with the next index / next tick.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("no chain data for asset {0:?}")]
    MissingAsset(volx_shared_types::Asset),
    #[error("no near expiry in [7d, 30d) for asset {0:?}")]
    NoNearExpiry(volx_shared_types::Asset),
    #[error("no next expiry > 30d for asset {0:?}")]
    NoNextExpiry(volx_shared_types::Asset),
    #[error("neither near nor next expiry available for asset {0:?}")]
    NoBracket(volx_shared_types::Asset),
    #[error("strip build failed (near): {0}")]
    NearStrip(BuildError),
    #[error("strip build failed (next): {0}")]
    NextStrip(BuildError),
    #[error("variance integral failed (near): {0}")]
    NearVariance(VarianceError),
    #[error("variance integral failed (next): {0}")]
    NextVariance(VarianceError),
    #[error("interpolation failed: {0}")]
    Interp(#[from] crate::interpolate::InterpError),
    #[error("BVOL conversion produced non-finite / negative σ²_30d")]
    Bvol,
}

impl SnapshotError {
    /// Stable label string for the
    /// `volx_engine_snapshot_rejected_total{reason}` counter.
    #[must_use]
    pub const fn as_label(&self) -> &'static str {
        match self {
            Self::MissingAsset(_) => "missing_asset",
            Self::NoNearExpiry(_) => "no_near_expiry",
            Self::NoNextExpiry(_) => "no_next_expiry",
            Self::NoBracket(_) => "no_bracket",
            Self::NearStrip(_) => "near_strip",
            Self::NextStrip(_) => "next_strip",
            Self::NearVariance(_) => "near_variance",
            Self::NextVariance(_) => "next_variance",
            Self::Interp(_) => "interp",
            Self::Bvol => "bvol",
        }
    }
}

/// Full pipeline result: the published [`IndexValue`] plus the two
/// `Strip`s that produced it. The strips travel to the scheduler so
/// it can persist them to the Redis `index:{id}:last_strip` key for
/// the `/v1/options/strip` transparency endpoint (#23).
#[derive(Debug)]
pub struct SnapshotResult {
    pub value: IndexValue,
    pub near: Strip,
    pub next: Strip,
}

/// Run the full pipeline for one index. Returns the published
/// [`IndexValue`] plus the near + next strips (for downstream
/// persistence).
pub fn run_snapshot(
    chains: &AssetChains,
    index: IndexId,
    now: OffsetDateTime,
) -> Result<SnapshotResult, SnapshotError> {
    let asset = index.asset();
    let per_asset = chains
        .get(&asset)
        .ok_or(SnapshotError::MissingAsset(asset))?;

    let (near_chain, next_chain) =
        pick_expiries(per_asset).ok_or_else(|| pick_failure(per_asset, asset))?;

    let near_strip = build_strip(near_chain).map_err(SnapshotError::NearStrip)?;
    let next_strip = build_strip(next_chain).map_err(SnapshotError::NextStrip)?;

    let near_sigma_sq = variance_t(&near_strip).map_err(SnapshotError::NearVariance)?;
    let next_sigma_sq = variance_t(&next_strip).map_err(SnapshotError::NextVariance)?;

    let sigma_sq_30d = interpolate_30d(
        ExpiryVariance::from_years(near_sigma_sq, near_strip.time_to_expiry),
        ExpiryVariance::from_years(next_sigma_sq, next_strip.time_to_expiry),
    )?;

    let bvol_value = bvol(sigma_sq_30d).ok_or(SnapshotError::Bvol)?;

    let strip_hash = compute_strip_hash(&near_strip, &next_strip);
    let confidence = confidence_from_strips(&near_strip, &next_strip);

    debug!(
        index_id = ?index,
        value = bvol_value,
        confidence,
        near_t = near_strip.time_to_expiry.0,
        next_t = next_strip.time_to_expiry.0,
        "snapshot computed"
    );

    Ok(SnapshotResult {
        value: IndexValue {
            index_id: index,
            value: bvol_value,
            confidence,
            strip_hash,
            ts: now,
        },
        near: near_strip,
        next: next_strip,
    })
}

/// §4.1 expiry picker. Returns `(near, next)` references into the
/// caller-owned map, or `None` if the constraints aren't satisfied.
///
/// Boundary note: an expiry exactly at `T_BRACKET` (`30d / 365`) falls
/// through both arms — `[7d, 30d)` excludes the upper bound and the
/// `t > T_BRACKET` arm requires strict greater-than. Both per §4.1
/// ("near in `[7d, 30d)`, next `> 30d`"), so an instrument with TTE
/// exactly 30d matches neither and is silently skipped. In practice
/// Deribit publishes 8:00-UTC-Friday expiries so the probability of
/// hitting this boundary is effectively zero, but the spec is
/// deliberate and we honour it.
fn pick_expiries(
    per_asset: &HashMap<OffsetDateTime, ExpiryChain>,
) -> Option<(&ExpiryChain, &ExpiryChain)> {
    let mut near: Option<&ExpiryChain> = None;
    let mut next: Option<&ExpiryChain> = None;
    for chain in per_asset.values() {
        let t = chain.time_to_expiry.0;
        if (T_NEAR_MIN..T_BRACKET).contains(&t) {
            // largest near: keep the chain with the bigger t
            if near.is_none_or(|c| t > c.time_to_expiry.0) {
                near = Some(chain);
            }
        } else if t > T_BRACKET {
            // smallest next: keep the chain with the smaller t
            if next.is_none_or(|c| t < c.time_to_expiry.0) {
                next = Some(chain);
            }
        }
    }
    match (near, next) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    }
}

/// Decide which variant of `NoNear / NoNext / NoBracket` to raise
/// when `pick_expiries` fails. Lets the caller produce a precise
/// reason label without doing the per-side scan twice. Distinguishing
/// "no near + no next" from "one missing" matters for Grafana
/// dashboards keyed on `reason` — between roll dates both legs can
/// disappear simultaneously and aggregating that case with a single
/// `no_near_expiry` would hide a different operational state.
fn pick_failure(
    per_asset: &HashMap<OffsetDateTime, ExpiryChain>,
    asset: volx_shared_types::Asset,
) -> SnapshotError {
    let has_near = per_asset
        .values()
        .any(|c| (T_NEAR_MIN..T_BRACKET).contains(&c.time_to_expiry.0));
    let has_next = per_asset.values().any(|c| c.time_to_expiry.0 > T_BRACKET);
    match (has_near, has_next) {
        (true, false) => SnapshotError::NoNextExpiry(asset),
        (false, true) => SnapshotError::NoNearExpiry(asset),
        // `(false, false)` is the legitimate no-bracket case (e.g.
        // between roll dates); `(true, true)` is logically unreachable
        // because then `pick_expiries` would have succeeded — fold
        // both into `NoBracket` so the metric carries the bug-signal
        // for the unreachable branch.
        _ => SnapshotError::NoBracket(asset),
    }
}

/// Audit hash: SHA-256 over the concatenated little-endian f64 bytes of
/// every dense-grid strike from both strips, plus the two forwards and
/// K₀s. Same snapshot → same hash; any drift in the input quotes
/// changes it. The 32-byte output maps directly to the
/// `index_ticks.strip_hash FixedString(32)` column.
fn compute_strip_hash(near: &Strip, next: &Strip) -> StripHash {
    let mut h = Sha256::new();
    h.update(near.forward.to_le_bytes());
    h.update(near.k_zero.to_le_bytes());
    h.update(near.time_to_expiry.0.to_le_bytes());
    for q in &near.quotes {
        h.update(q.strike.to_le_bytes());
        h.update(q.q_usd.to_le_bytes());
        h.update(q.iv.to_le_bytes());
    }
    h.update(next.forward.to_le_bytes());
    h.update(next.k_zero.to_le_bytes());
    h.update(next.time_to_expiry.0.to_le_bytes());
    for q in &next.quotes {
        h.update(q.strike.to_le_bytes());
        h.update(q.q_usd.to_le_bytes());
        h.update(q.iv.to_le_bytes());
    }
    let bytes: [u8; 32] = h.finalize().into();
    StripHash(bytes)
}

/// Bare-bones confidence proxy. Both strips are always 801-point dense
/// grids by construction (the strip builder guarantees that), so the
/// useful per-snapshot signal is the **listed-strike** count behind
/// the spline fit — but we don't track that through the `Strip` type
/// today. For #20 we publish a constant `1.0` and document the
/// follow-up; the column carries forward so a future PR can promote
/// the field without a schema migration.
const fn confidence_from_strips(_near: &Strip, _next: &Strip) -> f64 {
    1.0
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::bs::{call_price, put_price};
    use crate::strip::{ChainLeg, ExpiryChain};
    use volx_shared_types::Asset;
    use volx_shared_types::units::Years;

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

    fn build_chains_for_btc(near_t: f64, next_t: f64, iv: f64) -> AssetChains {
        let mut chains: AssetChains = HashMap::new();
        let per_asset = chains.entry(Asset::Btc).or_default();
        // Synthesize fake expiry timestamps; they just need to be
        // distinct so the HashMap keys don't collide.
        let near_expiry = time::macros::datetime!(2026-06-01 08:00:00 UTC);
        let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
        per_asset.insert(near_expiry, flat_iv_chain(100.0, 4.0, 30, near_t, iv));
        per_asset.insert(next_expiry, flat_iv_chain(100.0, 4.0, 30, next_t, iv));
        chains
    }

    #[test]
    fn happy_path_produces_bvol_near_iv() {
        // Flat-IV chain at 0.5: BVOL ≈ 100·iv = 50 (Carr-Madan recovers
        // iv² in the integration limit). The §4.1 pair brackets 30d.
        let near_t = 14.0 / 365.0; // 14 days
        let next_t = 60.0 / 365.0; // 60 days
        let chains = build_chains_for_btc(near_t, next_t, 0.5);
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        let res = run_snapshot(&chains, IndexId::Bvol, now).unwrap();
        assert_eq!(res.value.index_id, IndexId::Bvol);
        // Tolerance is loose (5 vol points) because the synthetic
        // chain's wing truncation feeds into both σ² components.
        assert!(
            (res.value.value - 50.0).abs() < 5.0,
            "BVOL={} (expected ≈ 50)",
            res.value.value
        );
        assert!(res.value.confidence >= 0.0 && res.value.confidence <= 1.0);
        // Strips travel out for the #23 strip-persist path.
        assert_eq!(res.near.quotes.len(), crate::DENSE_GRID_POINTS);
        assert_eq!(res.next.quotes.len(), crate::DENSE_GRID_POINTS);
    }

    #[test]
    fn rejects_when_no_near_expiry() {
        // Only a next expiry — no listed expiry in [7d, 30d).
        let mut chains: AssetChains = HashMap::new();
        let per_asset = chains.entry(Asset::Btc).or_default();
        let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
        per_asset.insert(
            next_expiry,
            flat_iv_chain(100.0, 4.0, 30, 60.0 / 365.0, 0.5),
        );
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        match run_snapshot(&chains, IndexId::Bvol, now) {
            Err(SnapshotError::NoNearExpiry(_)) => {}
            other => panic!("expected NoNearExpiry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_when_no_next_expiry() {
        // Only a near expiry — no listed expiry > 30d.
        let mut chains: AssetChains = HashMap::new();
        let per_asset = chains.entry(Asset::Btc).or_default();
        let near_expiry = time::macros::datetime!(2026-06-01 08:00:00 UTC);
        per_asset.insert(
            near_expiry,
            flat_iv_chain(100.0, 4.0, 30, 14.0 / 365.0, 0.5),
        );
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        match run_snapshot(&chains, IndexId::Bvol, now) {
            Err(SnapshotError::NoNextExpiry(_)) => {}
            other => panic!("expected NoNextExpiry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_when_asset_missing() {
        let chains: AssetChains = HashMap::new();
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        match run_snapshot(&chains, IndexId::Bvol, now) {
            Err(SnapshotError::MissingAsset(Asset::Btc)) => {}
            other => panic!("expected MissingAsset(Btc), got {other:?}"),
        }
    }

    #[test]
    fn picker_chooses_largest_near_and_smallest_next() {
        let mut chains: AssetChains = HashMap::new();
        let per_asset = chains.entry(Asset::Btc).or_default();
        // Three near candidates (10d, 20d, 28d) — must pick 28d.
        per_asset.insert(
            time::macros::datetime!(2026-06-04 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 10.0 / 365.0, 0.4),
        );
        per_asset.insert(
            time::macros::datetime!(2026-06-14 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 20.0 / 365.0, 0.45),
        );
        per_asset.insert(
            time::macros::datetime!(2026-06-22 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 28.0 / 365.0, 0.5),
        );
        // Two next candidates (35d, 60d) — must pick 35d.
        per_asset.insert(
            time::macros::datetime!(2026-06-29 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 35.0 / 365.0, 0.5),
        );
        per_asset.insert(
            time::macros::datetime!(2026-07-24 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 60.0 / 365.0, 0.6),
        );
        let (near, next) = pick_expiries(per_asset).unwrap();
        assert!((near.time_to_expiry.0 - 28.0 / 365.0).abs() < 1e-12);
        assert!((next.time_to_expiry.0 - 35.0 / 365.0).abs() < 1e-12);
    }

    #[test]
    fn strip_hash_is_deterministic() {
        let chains = build_chains_for_btc(14.0 / 365.0, 60.0 / 365.0, 0.5);
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        let a = run_snapshot(&chains, IndexId::Bvol, now).unwrap();
        let b = run_snapshot(&chains, IndexId::Bvol, now).unwrap();
        assert_eq!(a.value.strip_hash, b.value.strip_hash);
    }

    #[test]
    fn strip_hash_changes_when_iv_changes() {
        let chains_lo = build_chains_for_btc(14.0 / 365.0, 60.0 / 365.0, 0.4);
        let chains_hi = build_chains_for_btc(14.0 / 365.0, 60.0 / 365.0, 0.5);
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        let a = run_snapshot(&chains_lo, IndexId::Bvol, now).unwrap();
        let b = run_snapshot(&chains_hi, IndexId::Bvol, now).unwrap();
        assert_ne!(a.value.strip_hash, b.value.strip_hash);
    }

    /// Lock the wire-format `reason` labels — dashboards key on them.
    #[test]
    fn snapshot_error_labels_are_stable() {
        assert_eq!(
            SnapshotError::MissingAsset(Asset::Btc).as_label(),
            "missing_asset"
        );
        assert_eq!(
            SnapshotError::NoNearExpiry(Asset::Btc).as_label(),
            "no_near_expiry"
        );
        assert_eq!(
            SnapshotError::NoNextExpiry(Asset::Btc).as_label(),
            "no_next_expiry"
        );
        assert_eq!(
            SnapshotError::NoBracket(Asset::Btc).as_label(),
            "no_bracket"
        );
    }

    /// Regression for review MED-3: when *both* near and next are
    /// missing, the picker failure must surface as `NoBracket` so the
    /// Grafana counter aggregates this case separately from
    /// "only-one-missing".
    #[test]
    fn rejects_with_no_bracket_when_both_legs_missing() {
        let mut chains: AssetChains = HashMap::new();
        let per_asset = chains.entry(Asset::Btc).or_default();
        // A 3-day expiry — below T_NEAR_MIN (7d) — matches neither
        // arm of the §4.1 picker.
        per_asset.insert(
            time::macros::datetime!(2026-05-28 08:00:00 UTC),
            flat_iv_chain(100.0, 4.0, 30, 3.0 / 365.0, 0.5),
        );
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        match run_snapshot(&chains, IndexId::Bvol, now) {
            Err(SnapshotError::NoBracket(_)) => {}
            other => panic!("expected NoBracket, got {other:?}"),
        }
    }
}
