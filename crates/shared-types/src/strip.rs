//! Per-expiry option strip — the engine's input to the variance integral.
//!
//! A `Strip` is the output of the strip-builder stage (issue #17): forward
//! priced via put-call parity (§4.2), `k_zero` chosen on the dense grid
//! (§4.5), and `quotes` carrying the Carr-Madan OTM price `Q(K)` and the
//! fitted IV at every dense-grid strike (§4.3 / §4.5).

use serde::{Deserialize, Deserializer, Serialize};

use crate::units::Years;

/// Minimum number of strikes a `Strip` must carry for the variance integral
/// to be defensible (METHODOLOGY.md §3.2 + §5: "require ≥ 5 valid strikes;
/// otherwise reject the expiry"). Constant lives here so the strip-builder
/// (#17) and any cache / replay path consume the same threshold instead of
/// re-spelling the magic number.
pub const MIN_STRIP_QUOTES: usize = 5;

/// Lower clamp on fitted IV from METHODOLOGY.md §4.3 step 6.
pub const MIN_FITTED_IV: f64 = 1e-4;
/// Upper clamp on fitted IV from METHODOLOGY.md §4.3 step 6.
pub const MAX_FITTED_IV: f64 = 5.0;

/// One point on the dense-grid Carr-Madan kernel.
///
/// `q_usd` is the OTM USD option price at `strike`, with the K₀ averaging
/// rule (§4.5) already applied — there is no separate call / put leg at this
/// layer.
///
/// Invariants enforced at deserialize time:
/// - `strike`  finite and `> 0`,
/// - `q_usd`   finite and `>= 0` (Carr-Madan OTM price is non-negative),
/// - `iv`      finite and in `[MIN_FITTED_IV, MAX_FITTED_IV]` (§4.3 step 6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StripQuote {
    #[serde(deserialize_with = "de_positive_finite")]
    pub strike: f64,
    #[serde(deserialize_with = "de_finite_non_negative")]
    pub q_usd: f64,
    #[serde(deserialize_with = "de_fitted_iv")]
    pub iv: f64,
}

/// Per-expiry strip ready for the variance integral (§4.5).
///
/// Deserialize rejects a strip with fewer than [`MIN_STRIP_QUOTES`] entries
/// so a corrupted cache / replay payload cannot poison the integral.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strip {
    #[serde(deserialize_with = "de_positive_finite")]
    pub forward: f64,
    #[serde(deserialize_with = "de_positive_finite")]
    pub k_zero: f64,
    pub time_to_expiry: Years,
    #[serde(deserialize_with = "de_min_size_quotes")]
    pub quotes: Vec<StripQuote>,
}

fn de_positive_finite<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(de)?;
    if !v.is_finite() || v <= 0.0 {
        return Err(serde::de::Error::custom(format!(
            "expected finite > 0, got {v}"
        )));
    }
    Ok(v)
}

fn de_finite_non_negative<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(de)?;
    if !v.is_finite() || v < 0.0 {
        return Err(serde::de::Error::custom(format!(
            "expected finite >= 0, got {v}"
        )));
    }
    Ok(v)
}

fn de_fitted_iv<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(de)?;
    if !v.is_finite() || !(MIN_FITTED_IV..=MAX_FITTED_IV).contains(&v) {
        return Err(serde::de::Error::custom(format!(
            "fitted IV must be in [{MIN_FITTED_IV}, {MAX_FITTED_IV}], got {v}"
        )));
    }
    Ok(v)
}

fn de_min_size_quotes<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<StripQuote>, D::Error> {
    let v = Vec::<StripQuote>::deserialize(de)?;
    if v.len() < MIN_STRIP_QUOTES {
        return Err(serde::de::Error::custom(format!(
            "Strip.quotes must have >= {MIN_STRIP_QUOTES} entries (METHODOLOGY §3.2), got {}",
            v.len()
        )));
    }
    Ok(v)
}
