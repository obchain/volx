//! Sliding-window tick deduplication (issue #13).
//!
//! Drops a tick if the same `(venue, instrument, received_at)` has been
//! seen inside [`NormalizerConfig::dedup_window_secs`]. "Instrument" for an
//! options tick is the tuple `(asset, expiry, strike, kind)` — see
//! [`DedupKey`]. Time resolution is millisecond (matches the timestamp
//! precision METHODOLOGY.md §5 commits to).
//!
//! Eviction is two-layered:
//! 1. **Time-window** — on every `check`, entries older than `window` are
//!    popped off the front of the FIFO queue.
//! 2. **Size cap** — a hard limit on `seen.len()` acts as a memory safety
//!    net under burst loads where time-based eviction alone would balloon.
//!
//! Each detected duplicate increments
//! `volx_options_ticks_deduped_total{venue}` via the `metrics` facade.
//! The Prometheus exporter (#11) reads from the same facade without
//! touching emit sites here.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use time::OffsetDateTime;
use tracing::trace;
use volx_shared_types::{Asset, OptionKind, OptionTick, Venue};

/// Compact dedup identity for one option market update.
///
/// `strike_bits = f64::to_bits()` because `f64` does not implement `Hash` /
/// `Eq` directly. `to_bits` is a deterministic transmute and gives
/// bit-exact key equality, which is what dedup needs (a tick with strike
/// `1.0 + 1e-12` is not the same instrument as strike `1.0`, even though
/// the float diff is invisible to humans).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DedupKey {
    pub venue: Venue,
    pub asset: Asset,
    pub expiry_unix_ms: i64,
    pub strike_bits: u64,
    pub kind: OptionKind,
    pub received_at_unix_ms: i64,
}

impl DedupKey {
    /// Build the key from an [`OptionTick`].
    #[must_use]
    pub fn from_tick(tick: &OptionTick) -> Self {
        Self {
            venue: tick.venue,
            asset: tick.asset,
            expiry_unix_ms: unix_ms(tick.expiry),
            strike_bits: tick.strike.to_bits(),
            kind: tick.kind,
            received_at_unix_ms: unix_ms(tick.received_at),
        }
    }
}

fn unix_ms(t: OffsetDateTime) -> i64 {
    // `unix_timestamp_nanos` returns i128; div 1_000_000 fits in i64 for
    // any plausible timestamp (i64 ms range is ~292 million years).
    let nanos = t.unix_timestamp_nanos();
    i64::try_from(nanos / 1_000_000).unwrap_or(i64::MAX)
}

/// Outcome of running the deduper on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    /// First time we have seen this `(venue, instrument, ts)`. Forward
    /// the tick downstream.
    Fresh,
    /// Duplicate of a tick already in the window. Drop.
    Duplicate,
}

/// Sliding-window duplicate detector.
///
/// **Not** thread-safe by itself — wrap in `Arc<Mutex<_>>` or run inside a
/// single-task pipeline. The current ingestion topology drains one venue
/// task into one normalizer/dedup stage; no contention.
#[derive(Debug)]
pub struct Deduper {
    window: Duration,
    max_entries: usize,
    seen: HashSet<DedupKey>,
    /// FIFO of `(received_at, key)` in insertion order. We pop from the
    /// front during time-window eviction and during size-cap enforcement.
    queue: VecDeque<(OffsetDateTime, DedupKey)>,
}

impl Deduper {
    /// Build a deduper with explicit window + cap. `window` must be a
    /// positive duration; a zero-or-negative value would cause every tick
    /// to be evicted before the duplicate check could see it. `max_entries`
    /// must be at least 1 so the size cap can never reject the very tick
    /// that triggered the insert.
    ///
    /// # Panics
    ///
    /// Panics on `window == Duration::ZERO` or `max_entries == 0` —
    /// programmer errors that should be caught at the first start, not
    /// silently at the first tick.
    #[must_use]
    pub fn new(window: Duration, max_entries: usize) -> Self {
        assert!(
            !window.is_zero(),
            "Deduper window must be positive (got zero)"
        );
        assert!(max_entries >= 1, "Deduper max_entries must be >= 1 (got 0)");
        Self {
            window,
            max_entries,
            seen: HashSet::new(),
            queue: VecDeque::new(),
        }
    }

    /// Build using the project defaults (60 s window, 240 k cap). Single
    /// source of truth: delegate through `NormalizerConfig::default()` so a
    /// future change to a default in one place can't drift between the two.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::from_config(&crate::config::NormalizerConfig::default())
    }

    /// Build from the normalizer config.
    #[must_use]
    pub fn from_config(config: &crate::config::NormalizerConfig) -> Self {
        Self::new(config.dedup_window(), config.dedup_max_entries)
    }

    /// Number of distinct keys currently in the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` if the window is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Check whether `tick` is a duplicate of one already inside the window.
    ///
    /// `now` is supplied by the caller so tests are deterministic; in
    /// production the ingestion pipeline passes `OffsetDateTime::now_utc()`.
    pub fn check(&mut self, tick: &OptionTick, now: OffsetDateTime) -> DedupOutcome {
        self.evict_old(now);
        let key = DedupKey::from_tick(tick);
        if self.seen.contains(&key) {
            metrics::counter!(
                "volx_options_ticks_deduped_total",
                "venue" => venue_label(tick.venue),
            )
            .increment(1);
            trace!(venue = venue_label(tick.venue), "tick deduplicated");
            return DedupOutcome::Duplicate;
        }
        self.seen.insert(key);
        self.queue.push_back((tick.received_at, key));
        self.enforce_size_cap();
        DedupOutcome::Fresh
    }

    /// Pop entries off the front whose `received_at` is at or older than
    /// `now − window`. The `<=` boundary is deliberate: an entry whose age
    /// is *exactly* the window is treated as outside it (window is
    /// half-open `[now − window, now)`). O(k) where k is the number of
    /// expired entries.
    fn evict_old(&mut self, now: OffsetDateTime) {
        let cutoff = now - time::Duration::try_from(self.window).unwrap_or(time::Duration::ZERO);
        while let Some(&(ts, _)) = self.queue.front() {
            if ts <= cutoff {
                // pop_front is guaranteed to succeed: we just peeked and
                // nothing else mutates the queue between the two ops.
                let (_, key) = self.queue.pop_front().expect("queue.front was Some");
                self.seen.remove(&key);
            } else {
                break;
            }
        }
    }

    /// Pop the oldest entries until size is under the cap. Runs after every
    /// fresh insert; under normal load this is a no-op because time-based
    /// eviction keeps `len` well below `max_entries`. By construction
    /// `seen.len() == queue.len()`, so `pop_front` is guaranteed to succeed
    /// whenever the loop condition is true.
    fn enforce_size_cap(&mut self) {
        while self.seen.len() > self.max_entries {
            let (_, key) = self
                .queue
                .pop_front()
                .expect("queue + seen invariant: equal length");
            self.seen.remove(&key);
        }
    }
}

/// Stable Prometheus `venue` label for the deduper counter. Keep in sync
/// with whatever the Venue serde label is so dashboards see one canonical
/// form across `volx_normalizer_filtered_total` and the dedup counter.
const fn venue_label(v: Venue) -> &'static str {
    match v {
        Venue::Deribit => "deribit",
        Venue::Okx => "okx",
        Venue::Bybit => "bybit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;
    use volx_shared_types::{Asset, OptionKind, OptionTick, Venue};

    fn tick_at(received_at: OffsetDateTime, strike: f64) -> OptionTick {
        OptionTick {
            venue: Venue::Deribit,
            asset: Asset::Btc,
            expiry: datetime!(2026-06-26 08:00:00 UTC),
            strike,
            kind: OptionKind::Call,
            bid: Some(2_500.0),
            ask: Some(2_550.0),
            mid: Some(2_525.0),
            iv: Some(0.65),
            underlying: 68_500.0,
            open_interest: 100.0,
            volume_24h: 10.0,
            received_at,
        }
    }

    fn t0() -> OffsetDateTime {
        datetime!(2026-05-25 12:00:00 UTC)
    }

    #[test]
    fn first_tick_is_fresh() {
        let mut d = Deduper::with_defaults();
        let t = tick_at(t0(), 70_000.0);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Fresh);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn same_tick_again_is_duplicate() {
        let mut d = Deduper::with_defaults();
        let t = tick_at(t0(), 70_000.0);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Fresh);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Duplicate);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn different_strike_is_fresh() {
        let mut d = Deduper::with_defaults();
        assert_eq!(d.check(&tick_at(t0(), 70_000.0), t0()), DedupOutcome::Fresh);
        assert_eq!(d.check(&tick_at(t0(), 80_000.0), t0()), DedupOutcome::Fresh);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn different_received_at_is_fresh() {
        // Same instrument, different timestamps = two distinct snapshots.
        let mut d = Deduper::with_defaults();
        assert_eq!(d.check(&tick_at(t0(), 70_000.0), t0()), DedupOutcome::Fresh);
        let later = t0() + time::Duration::milliseconds(100);
        assert_eq!(
            d.check(&tick_at(later, 70_000.0), later),
            DedupOutcome::Fresh
        );
    }

    #[test]
    fn entries_older_than_window_evicted() {
        // Insert at t0, then check the same key at t0 + 61 s. The cache
        // should have evicted the old entry, so the second check is
        // `Fresh` (not a duplicate) and `len` is back to 1.
        let mut d = Deduper::with_defaults();
        let t = tick_at(t0(), 70_000.0);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Fresh);
        let later = t0() + time::Duration::seconds(61);
        // The repeat tick is genuinely at the same `received_at`, but the
        // window relative to `now = later` evicted the original entry, so
        // the deduper has no memory of it.
        assert_eq!(d.check(&t, later), DedupOutcome::Fresh);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn duplicate_within_window_still_dropped() {
        // Boundary: same key 59 s later is still a duplicate.
        let mut d = Deduper::with_defaults();
        let t = tick_at(t0(), 70_000.0);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Fresh);
        let still_inside = t0() + time::Duration::seconds(59);
        assert_eq!(d.check(&t, still_inside), DedupOutcome::Duplicate);
    }

    #[test]
    fn size_cap_evicts_oldest() {
        // Tiny cap (3 entries). Insert 5 distinct strikes; oldest 2 must
        // be evicted, so only the newest 3 should remain. The first
        // strike should now be `Fresh` again because its key is gone.
        let mut d = Deduper::new(Duration::from_secs(60), 3);
        for k in 0..5 {
            assert_eq!(
                d.check(&tick_at(t0(), f64::from(70_000 + k)), t0()),
                DedupOutcome::Fresh
            );
        }
        assert_eq!(d.len(), 3);
        // Strike 70_000 was evicted; re-checking it now returns Fresh.
        assert_eq!(d.check(&tick_at(t0(), 70_000.0), t0()), DedupOutcome::Fresh);
    }

    #[test]
    fn dedup_key_separates_by_kind() {
        // Same strike + expiry + asset, call vs put = distinct instruments.
        let mut d = Deduper::with_defaults();
        let mut call = tick_at(t0(), 70_000.0);
        call.kind = OptionKind::Call;
        let mut put = tick_at(t0(), 70_000.0);
        put.kind = OptionKind::Put;
        assert_eq!(d.check(&call, t0()), DedupOutcome::Fresh);
        assert_eq!(d.check(&put, t0()), DedupOutcome::Fresh);
    }

    #[test]
    fn dedup_key_separates_by_venue() {
        // Same option contract, different exchanges = different ticks.
        let mut d = Deduper::with_defaults();
        let mut a = tick_at(t0(), 70_000.0);
        a.venue = Venue::Deribit;
        let mut b = tick_at(t0(), 70_000.0);
        b.venue = Venue::Okx;
        assert_eq!(d.check(&a, t0()), DedupOutcome::Fresh);
        assert_eq!(d.check(&b, t0()), DedupOutcome::Fresh);
    }

    #[test]
    fn strike_bit_exact_equality() {
        // Two strikes that differ by one ULP are distinct keys.
        let mut d = Deduper::with_defaults();
        let s = 70_000.0_f64;
        let next = f64::from_bits(s.to_bits() + 1);
        assert_eq!(d.check(&tick_at(t0(), s), t0()), DedupOutcome::Fresh);
        assert_eq!(d.check(&tick_at(t0(), next), t0()), DedupOutcome::Fresh);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn empty_after_full_eviction() {
        let mut d = Deduper::with_defaults();
        assert!(d.is_empty());
        assert_eq!(d.check(&tick_at(t0(), 70_000.0), t0()), DedupOutcome::Fresh);
        assert!(!d.is_empty());
        // Roll forward past the window; eviction is lazy, so trigger it
        // via another `check`.
        let later = t0() + time::Duration::seconds(120);
        let _ = d.check(&tick_at(later, 80_000.0), later);
        // The original 70k key is gone; only the 80k one remains.
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn unix_ms_truncates_to_millisecond() {
        // Fixture lands on the ms boundary; reading back at ms resolution
        // must match. Sub-millisecond components are *intentionally*
        // truncated by `unix_ms` so the assertion compares ms-truncated
        // values rather than the raw `OffsetDateTime`.
        let t = datetime!(2026-05-25 12:34:56.789_111_222 UTC);
        let ms = unix_ms(t);
        let expected_ms = i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap();
        assert_eq!(ms, expected_ms);
    }

    #[test]
    fn entry_exactly_at_window_boundary_is_evicted() {
        // METHODOLOGY interpretation: window is `[now − window, now)` —
        // an entry whose age is *exactly* the window length is treated as
        // outside it. A repeat at `t0 + 60 s` (= window) sees an empty
        // cache and is `Fresh`, not `Duplicate`.
        let mut d = Deduper::with_defaults();
        let t = tick_at(t0(), 70_000.0);
        assert_eq!(d.check(&t, t0()), DedupOutcome::Fresh);
        let at_boundary = t0() + time::Duration::seconds(60);
        assert_eq!(d.check(&t, at_boundary), DedupOutcome::Fresh);
        assert_eq!(d.len(), 1);
    }

    #[test]
    #[should_panic(expected = "must be positive")]
    fn zero_window_panics_at_construction() {
        let _ = Deduper::new(Duration::ZERO, 100);
    }

    #[test]
    #[should_panic(expected = "must be >= 1")]
    fn zero_max_entries_panics_at_construction() {
        let _ = Deduper::new(Duration::from_secs(60), 0);
    }

    #[test]
    fn from_config_uses_config_values() {
        let cfg = crate::config::NormalizerConfig {
            dedup_window_secs: 10.0,
            dedup_max_entries: 7,
            ..Default::default()
        };
        let d = Deduper::from_config(&cfg);
        assert_eq!(d.window, Duration::from_secs(10));
        assert_eq!(d.max_entries, 7);
    }
}
