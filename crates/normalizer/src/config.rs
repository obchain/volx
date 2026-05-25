//! Normalizer configuration — TOML-loadable thresholds for the per-tick
//! filter pipeline (issue #12, METHODOLOGY.md §3.1).
//!
//! Defaults match the methodology table; production deployments can override
//! by writing a `volx.toml` and parsing it via [`NormalizerConfig::from_toml_str`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default max tick age (seconds). METHODOLOGY.md §3.1 — staleness.
const DEFAULT_MAX_AGE_SECS: f64 = 5.0;
/// Default `(ask − bid) / mid` cap. METHODOLOGY.md §3.1 — spread filter.
const DEFAULT_MAX_SPREAD_RATIO: f64 = 0.30;
/// Default below-intrinsic tolerance (USD). METHODOLOGY.md §3.1.
const DEFAULT_INTRINSIC_TOLERANCE: f64 = 1e-9;

const fn default_max_age_secs() -> f64 {
    DEFAULT_MAX_AGE_SECS
}
const fn default_max_spread_ratio() -> f64 {
    DEFAULT_MAX_SPREAD_RATIO
}
const fn default_intrinsic_tolerance() -> f64 {
    DEFAULT_INTRINSIC_TOLERANCE
}

/// Thresholds for the per-tick filter pipeline.
///
/// `serde(default)` on every field means a partial TOML overrides only the
/// fields it mentions; everything else keeps the methodology default. This
/// keeps configs forward-compatible: future filters can land without
/// breaking existing deployments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizerConfig {
    /// Drop a quote whose `received_at` is older than this many seconds.
    #[serde(default = "default_max_age_secs")]
    pub max_age_secs: f64,
    /// Drop a quote whose `(ask − bid) / mid` exceeds this ratio.
    #[serde(default = "default_max_spread_ratio")]
    pub max_spread_ratio: f64,
    /// Below-intrinsic check tolerance (USD). Allows a tiny float-rounding
    /// margin so a mid exactly at intrinsic is not flagged.
    #[serde(default = "default_intrinsic_tolerance")]
    pub intrinsic_tolerance: f64,
}

impl Default for NormalizerConfig {
    fn default() -> Self {
        Self {
            max_age_secs: default_max_age_secs(),
            max_spread_ratio: default_max_spread_ratio(),
            intrinsic_tolerance: default_intrinsic_tolerance(),
        }
    }
}

impl NormalizerConfig {
    /// Parse a TOML string. Missing fields fall back to the methodology
    /// defaults; unknown fields are rejected (typo guard).
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// `max_age_secs` as a [`Duration`].
    #[must_use]
    pub fn max_age(&self) -> Duration {
        Duration::from_secs_f64(self.max_age_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_methodology() {
        let c = NormalizerConfig::default();
        assert!((c.max_age_secs - 5.0).abs() < 1e-12);
        assert!((c.max_spread_ratio - 0.30).abs() < 1e-12);
        assert!((c.intrinsic_tolerance - 1e-9).abs() < 1e-20);
    }

    #[test]
    fn toml_full_round_trip() {
        let src = r"
            max_age_secs        = 3.5
            max_spread_ratio    = 0.25
            intrinsic_tolerance = 1.0e-9
        ";
        let c = NormalizerConfig::from_toml_str(src).unwrap();
        assert!((c.max_age_secs - 3.5).abs() < 1e-12);
        assert!((c.max_spread_ratio - 0.25).abs() < 1e-12);
    }

    #[test]
    fn toml_partial_fills_defaults() {
        let src = "max_spread_ratio = 0.10";
        let c = NormalizerConfig::from_toml_str(src).unwrap();
        assert!((c.max_spread_ratio - 0.10).abs() < 1e-12);
        // others fell back to defaults
        assert!((c.max_age_secs - 5.0).abs() < 1e-12);
    }

    #[test]
    fn toml_empty_is_all_defaults() {
        let c = NormalizerConfig::from_toml_str("").unwrap();
        assert!((c.max_age_secs - 5.0).abs() < 1e-12);
    }

    #[test]
    fn toml_rejects_unknown_field() {
        let src = r"
            max_age_secs = 5.0
            mystery_knob = 42
        ";
        assert!(NormalizerConfig::from_toml_str(src).is_err());
    }

    #[test]
    fn max_age_duration_conversion() {
        let c = NormalizerConfig {
            max_age_secs: 2.5,
            ..Default::default()
        };
        assert_eq!(c.max_age(), Duration::from_millis(2_500));
    }
}
