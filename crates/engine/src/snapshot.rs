//! Snapshot orchestrator — takes per-venue, per-asset chains and
//! produces one [`IndexValue`] per index (BVOL / EVOL) per 60-second
//! tick (issues #20, #61).
//!
//! Pipeline per asset:
//!
//! 1. **Per-venue strip.** For each venue with a chain for this
//!    asset, run the §4.1 picker (near = largest listed expiry with
//!    `t_t ∈ [7d, 30d)`, next = smallest listed expiry with
//!    `t_t > 30d`), then the strip builder (§4.2 + §4.3 + §4.5),
//!    variance integral (§4.5), 30 d interpolation (§4.6) and BVOL
//!    conversion (§4.7). Venues that fail any step are recorded but
//!    skipped — the blend uses the survivors.
//! 2. **Cross-venue median blend (#61, `math_reference.md:71`).**
//!    The published value is [`crate::blend::median`] over the
//!    per-venue raw BVOLs. Single-venue degraded mode collapses to
//!    identity. With zero survivors the snapshot is rejected with
//!    [`SnapshotError::NoVenuesLive`].
//!
//! Tiebreak / determinism: per-venue values are iterated in
//! ascending alpha order on [`Venue::label`] before they are folded
//! into the audit hash, so the same input chains across two runs
//! produce the same `strip_hash`.
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
use tracing::{debug, warn};
use volx_shared_types::ids::{IndexId, Venue};
use volx_shared_types::index::{IndexValue, StripHash};
use volx_shared_types::strip::Strip;

use crate::blend;
use crate::chain::{AssetChains, MultiVenueChains};
use crate::interpolate::{ExpiryVariance, bvol, interpolate_30d};
use crate::strip::{BuildError, ExpiryChain, build_strip};
use crate::variance::{VarianceError, variance_t};

/// Time-to-expiry bounds (years). Hard-pinned from `METHODOLOGY.md`
/// §4.1; the §4.1 picker uses them to bracket 30 days.
const T_NEAR_MIN: f64 = 7.0 / 365.0;
const T_BRACKET: f64 = 30.0 / 365.0;

/// Why a single venue's per-venue snapshot can fail. Surfaced via
/// [`SnapshotResult::per_venue_errors`] so the scheduler can log /
/// count individual venue failures without failing the blended
/// publish — a single venue going dark must not stop the index when
/// other venues are live.
#[derive(Debug, thiserror::Error)]
pub enum PerVenueError {
    #[error("no near expiry in [7d, 30d)")]
    NoNearExpiry,
    #[error("no next expiry > 30d")]
    NoNextExpiry,
    #[error("neither near nor next expiry available")]
    NoBracket,
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

impl PerVenueError {
    /// Stable label string for the per-venue rejection counter
    /// (`volx_engine_per_venue_rejected_total{venue,reason}`).
    #[must_use]
    pub const fn as_label(&self) -> &'static str {
        match self {
            Self::NoNearExpiry => "no_near_expiry",
            Self::NoNextExpiry => "no_next_expiry",
            Self::NoBracket => "no_bracket",
            Self::NearStrip(_) => "near_strip",
            Self::NextStrip(_) => "next_strip",
            Self::NearVariance(_) => "near_variance",
            Self::NextVariance(_) => "next_variance",
            Self::Interp(_) => "interp",
            Self::Bvol => "bvol",
        }
    }
}

/// Why a per-index *blended* snapshot can fail. All variants are
/// non-fatal at the scheduler level — the per-tick driver logs +
/// counter-emits and continues with the next index / next tick.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("no chain data for asset {0:?} on any venue")]
    MissingAsset(volx_shared_types::Asset),
    #[error("every venue failed its per-venue snapshot for asset {asset:?}")]
    NoVenuesLive {
        asset: volx_shared_types::Asset,
        /// Per-venue reasons collected from the (failed) blend loop —
        /// surfaced so the scheduler can emit the per-venue counter
        /// even when *every* venue is dark, which is the
        /// highest-severity operational state and the one where the
        /// per-venue breakdown matters most.
        per_venue_errors: Vec<(Venue, PerVenueError)>,
    },
}

impl SnapshotError {
    /// Stable label string for the
    /// `volx_engine_snapshot_rejected_total{reason}` counter.
    #[must_use]
    pub const fn as_label(&self) -> &'static str {
        match self {
            Self::MissingAsset(_) => "missing_asset",
            Self::NoVenuesLive { .. } => "no_venues_live",
        }
    }
}

/// One venue's contribution to a blended snapshot: the per-venue raw
/// BVOL plus the two strips that produced it. Travels out so the
/// scheduler can persist the strips (Redis `index:{id}:last_strip`,
/// `/v1/options/strip` endpoint, #23) and so downstream issues
/// (#62 confidence, #63 outlier-drop) can read the per-venue
/// numbers.
#[derive(Debug, Clone)]
pub struct VenueSnapshot {
    pub venue: Venue,
    /// Per-venue raw BVOL — what the blend sees before the median.
    pub value: f64,
    pub near: Strip,
    pub next: Strip,
}

/// Full pipeline result: the blended [`IndexValue`] plus the per-venue
/// strips + raw values that produced it. The first per-venue snapshot
/// (in alpha order on [`Venue::label`]) doubles as the
/// `index:{id}:last_strip` payload until the strip-endpoint shape is
/// extended for the multi-venue world.
#[derive(Debug)]
pub struct SnapshotResult {
    pub value: IndexValue,
    pub per_venue: Vec<VenueSnapshot>,
    pub per_venue_errors: Vec<(Venue, PerVenueError)>,
}

impl SnapshotResult {
    /// Strips from the first venue (alpha order on `Venue.label()`).
    /// Used by the scheduler to populate the existing single-strip
    /// fanout sinks until the strip-endpoint shape is extended for
    /// multi-venue.
    #[must_use]
    pub fn primary_strips(&self) -> Option<(&Strip, &Strip)> {
        self.per_venue.first().map(|v| (&v.near, &v.next))
    }
}

/// Run the full pipeline for one index across every venue that has
/// chains for its asset, then median-blend the per-venue raw BVOLs.
///
/// # Errors
///
/// - [`SnapshotError::MissingAsset`] — no venue has any chain data
///   for the index's asset.
/// - [`SnapshotError::NoVenuesLive`] — at least one venue had chain
///   data but every venue's per-venue pipeline failed. The
///   per-venue reasons are still observable via
///   [`SnapshotResult::per_venue_errors`] if the caller wants them
///   (but `SnapshotResult` is not constructed in this case — the
///   caller should log the [`SnapshotError`] reason label).
pub fn run_snapshot(
    chains: &MultiVenueChains,
    index: IndexId,
    now: OffsetDateTime,
) -> Result<SnapshotResult, SnapshotError> {
    let asset = index.asset();

    // Collect per-venue contributions in alpha order on Venue.label()
    // so the iteration order — and therefore the strip_hash, the
    // alpha-tiebreak for the median, and the chosen primary_strips()
    // venue — is deterministic.
    let mut venues: Vec<(Venue, &AssetChains)> = chains
        .iter()
        .filter(|(_, per_venue)| per_venue.contains_key(&asset))
        .map(|(v, c)| (*v, c))
        .collect();
    if venues.is_empty() {
        return Err(SnapshotError::MissingAsset(asset));
    }
    venues.sort_by_key(|(v, _)| v.label());

    let mut per_venue: Vec<VenueSnapshot> = Vec::with_capacity(venues.len());
    let mut per_venue_errors: Vec<(Venue, PerVenueError)> = Vec::new();

    for (venue, asset_chains) in venues {
        match run_venue_snapshot(venue, asset_chains, asset) {
            Ok(vs) => per_venue.push(vs),
            Err(e) => {
                warn!(
                    venue = venue.label(),
                    index_id = ?index,
                    reason = e.as_label(),
                    error = %e,
                    "per-venue snapshot rejected"
                );
                per_venue_errors.push((venue, e));
            }
        }
    }

    if per_venue.is_empty() {
        return Err(SnapshotError::NoVenuesLive {
            asset,
            per_venue_errors,
        });
    }

    // Median over per-venue raw BVOLs. Empty case is unreachable
    // (handled above) but `median` returns Option so the unwrap is
    // total — keep it explicit rather than `expect`-panicking on a
    // shape we just guarded.
    let raw_values: Vec<f64> = per_venue.iter().map(|v| v.value).collect();
    let blended = blend::median(&raw_values).ok_or(SnapshotError::NoVenuesLive {
        asset,
        per_venue_errors: Vec::new(),
    })?;

    let strip_hash = compute_multi_venue_strip_hash(&per_venue);
    let confidence = confidence_from_strips(&per_venue);

    debug!(
        index_id = ?index,
        value = blended,
        confidence,
        venues_live = per_venue.len(),
        venues_failed = per_venue_errors.len(),
        "blended snapshot computed"
    );

    Ok(SnapshotResult {
        value: IndexValue {
            index_id: index,
            value: blended,
            confidence,
            strip_hash,
            ts: now,
        },
        per_venue,
        per_venue_errors,
    })
}

/// Run the per-venue pipeline for one (venue, asset) pair. Pulled out
/// of [`run_snapshot`] so the outer fn can iterate cleanly and so the
/// per-venue path is unit-testable against a single-venue
/// [`AssetChains`].
///
/// # Panics
///
/// Panics if `chains` does not contain `asset`. The only call site
/// (`run_snapshot`) pre-filters venues that lack the asset, so this
/// branch is unreachable in production. Asserting rather than
/// silently surfacing `NoBracket` (the previous behaviour) prevents
/// a future refactor from masking a call-site invariant break as a
/// roll-date metric.
fn run_venue_snapshot(
    venue: Venue,
    chains: &AssetChains,
    asset: volx_shared_types::Asset,
) -> Result<VenueSnapshot, PerVenueError> {
    let per_asset = chains
        .get(&asset)
        .expect("run_snapshot pre-filters venues by asset; key must be present");

    let (near_chain, next_chain) =
        pick_expiries(per_asset).ok_or_else(|| pick_failure(per_asset))?;

    let near_strip = build_strip(near_chain).map_err(PerVenueError::NearStrip)?;
    let next_strip = build_strip(next_chain).map_err(PerVenueError::NextStrip)?;

    let near_sigma_sq = variance_t(&near_strip).map_err(PerVenueError::NearVariance)?;
    let next_sigma_sq = variance_t(&next_strip).map_err(PerVenueError::NextVariance)?;

    let sigma_sq_30d = interpolate_30d(
        ExpiryVariance::from_years(near_sigma_sq, near_strip.time_to_expiry),
        ExpiryVariance::from_years(next_sigma_sq, next_strip.time_to_expiry),
    )?;

    let value = bvol(sigma_sq_30d).ok_or(PerVenueError::Bvol)?;

    Ok(VenueSnapshot {
        venue,
        value,
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
/// when `pick_expiries` fails for a single venue. Lets the caller
/// produce a precise reason label without doing the per-side scan
/// twice. Distinguishing "no near + no next" from "one missing"
/// matters for Grafana dashboards keyed on `reason` — between roll
/// dates both legs can disappear simultaneously and aggregating that
/// case with a single `no_near_expiry` would hide a different
/// operational state.
fn pick_failure(per_asset: &HashMap<OffsetDateTime, ExpiryChain>) -> PerVenueError {
    let has_near = per_asset
        .values()
        .any(|c| (T_NEAR_MIN..T_BRACKET).contains(&c.time_to_expiry.0));
    let has_next = per_asset.values().any(|c| c.time_to_expiry.0 > T_BRACKET);
    match (has_near, has_next) {
        (true, false) => PerVenueError::NoNextExpiry,
        (false, true) => PerVenueError::NoNearExpiry,
        // `(false, false)` is the legitimate no-bracket case (e.g.
        // between roll dates); `(true, true)` is logically unreachable
        // because then `pick_expiries` would have succeeded — fold
        // both into `NoBracket` so the metric carries the bug-signal
        // for the unreachable branch.
        _ => PerVenueError::NoBracket,
    }
}

/// Audit hash: SHA-256 over every per-venue strip pair in alpha
/// order on [`Venue::label`]. The venue label is folded into the
/// hash too so a venue going dark (or coming live) changes the hash
/// — that's the audit signal a verifier needs to distinguish "same
/// input + same venue mix" from "same input + different venue
/// composition produced the same blend by coincidence".
///
/// The 32-byte output maps directly to the
/// `index_ticks.strip_hash FixedString(32)` column.
fn compute_multi_venue_strip_hash(per_venue: &[VenueSnapshot]) -> StripHash {
    let mut h = Sha256::new();
    for vs in per_venue {
        h.update(vs.venue.label().as_bytes());
        // Fixed null-byte separator between the variable-length venue
        // label and the strip's f64 bytes. Current Venue labels
        // (bybit / deribit / okx) have no prefix overlap, but adding
        // a future label that is a prefix of another would otherwise
        // create a theoretical collision between `h("ok") + h(strip
        // whose first byte is b'x')` and `h("okx") + h(strip')`. The
        // separator costs one byte per venue and makes the
        // construction prefix-free for the closed enum.
        h.update([0u8]);
        fold_strip_into_hash(&mut h, &vs.near);
        fold_strip_into_hash(&mut h, &vs.next);
    }
    let bytes: [u8; 32] = h.finalize().into();
    StripHash(bytes)
}

fn fold_strip_into_hash(h: &mut Sha256, s: &Strip) {
    h.update(s.forward.to_le_bytes());
    h.update(s.k_zero.to_le_bytes());
    h.update(s.time_to_expiry.0.to_le_bytes());
    for q in &s.quotes {
        h.update(q.strike.to_le_bytes());
        h.update(q.q_usd.to_le_bytes());
        h.update(q.iv.to_le_bytes());
    }
}

/// Bare-bones confidence proxy. Both strips per venue are always
/// 801-point dense grids by construction; the useful per-snapshot
/// signal is the **listed-strike** count behind the spline fit — and
/// (post-#61) the count of live venues — but neither is tracked on
/// the `Strip` type today. For #20 + #61 we publish a constant `1.0`
/// and the dedicated confidence issue (#62) populates this field.
/// The column carries forward so #62 can promote the value without a
/// schema migration.
const fn confidence_from_strips(_per_venue: &[VenueSnapshot]) -> f64 {
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

    /// Single-venue [`MultiVenueChains`] (Deribit), BTC, one near + one
    /// next expiry. Backed the single-venue tests pre-#61; reused here
    /// to lock the degraded-mode behaviour.
    fn build_single_venue_btc_chains(near_t: f64, next_t: f64, iv: f64) -> MultiVenueChains {
        let mut out: MultiVenueChains = HashMap::new();
        let per_venue = out.entry(Venue::Deribit).or_default();
        let per_asset = per_venue.entry(Asset::Btc).or_default();
        let near_expiry = time::macros::datetime!(2026-06-01 08:00:00 UTC);
        let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
        per_asset.insert(near_expiry, flat_iv_chain(100.0, 4.0, 30, near_t, iv));
        per_asset.insert(next_expiry, flat_iv_chain(100.0, 4.0, 30, next_t, iv));
        out
    }

    #[test]
    fn happy_path_produces_bvol_near_iv() {
        // Flat-IV chain at 0.5: BVOL ≈ 100·iv = 50 (Carr-Madan recovers
        // iv² in the integration limit). The §4.1 pair brackets 30d.
        let near_t = 14.0 / 365.0; // 14 days
        let next_t = 60.0 / 365.0; // 60 days
        let chains = build_single_venue_btc_chains(near_t, next_t, 0.5);
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
        // Single-venue degraded mode: per_venue has one entry whose
        // strips drive the publish.
        assert_eq!(res.per_venue.len(), 1);
        assert_eq!(res.per_venue[0].venue, Venue::Deribit);
        let (near, next) = res.primary_strips().unwrap();
        assert_eq!(near.quotes.len(), crate::DENSE_GRID_POINTS);
        assert_eq!(next.quotes.len(), crate::DENSE_GRID_POINTS);
    }

    #[test]
    fn rejects_with_no_venues_live_when_only_venue_has_no_near() {
        // One venue, one expiry only (a next) — the per-venue
        // pipeline fails (`NoNearExpiry`) and the blend has no
        // survivors.
        let mut chains: MultiVenueChains = HashMap::new();
        let per_venue = chains.entry(Venue::Deribit).or_default();
        let per_asset = per_venue.entry(Asset::Btc).or_default();
        let next_expiry = time::macros::datetime!(2026-07-01 08:00:00 UTC);
        per_asset.insert(
            next_expiry,
            flat_iv_chain(100.0, 4.0, 30, 60.0 / 365.0, 0.5),
        );
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        match run_snapshot(&chains, IndexId::Bvol, now) {
            Err(SnapshotError::NoVenuesLive {
                asset: Asset::Btc,
                per_venue_errors,
            }) => {
                // Per-venue breakdown must travel out so the
                // scheduler can fire the per-venue counter on the
                // total-failure path.
                assert_eq!(per_venue_errors.len(), 1);
                assert_eq!(per_venue_errors[0].0, Venue::Deribit);
                assert_eq!(per_venue_errors[0].1.as_label(), "no_near_expiry");
            }
            other => panic!("expected NoVenuesLive(Btc), got {other:?}"),
        }
    }

    #[test]
    fn rejects_when_asset_missing_on_every_venue() {
        let chains: MultiVenueChains = HashMap::new();
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
        let chains = build_single_venue_btc_chains(14.0 / 365.0, 60.0 / 365.0, 0.5);
        let now = time::macros::datetime!(2026-05-25 12:00:00 UTC);
        let a = run_snapshot(&chains, IndexId::Bvol, now).unwrap();
        let b = run_snapshot(&chains, IndexId::Bvol, now).unwrap();
        assert_eq!(a.value.strip_hash, b.value.strip_hash);
    }

    #[test]
    fn strip_hash_changes_when_iv_changes() {
        let chains_lo = build_single_venue_btc_chains(14.0 / 365.0, 60.0 / 365.0, 0.4);
        let chains_hi = build_single_venue_btc_chains(14.0 / 365.0, 60.0 / 365.0, 0.5);
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
            SnapshotError::NoVenuesLive {
                asset: Asset::Btc,
                per_venue_errors: Vec::new(),
            }
            .as_label(),
            "no_venues_live"
        );
    }

    /// Per-venue error labels are themselves a wire-format contract
    /// because the per-venue counter
    /// (`volx_engine_per_venue_rejected_total{venue,reason}`) is
    /// dashboarded the same way.
    #[test]
    fn per_venue_error_labels_are_stable() {
        assert_eq!(PerVenueError::NoNearExpiry.as_label(), "no_near_expiry");
        assert_eq!(PerVenueError::NoNextExpiry.as_label(), "no_next_expiry");
        assert_eq!(PerVenueError::NoBracket.as_label(), "no_bracket");
        assert_eq!(PerVenueError::Bvol.as_label(), "bvol");
    }
}
