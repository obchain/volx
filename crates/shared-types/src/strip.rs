//! Per-expiry option strip — the engine's input to the variance integral.
//!
//! A `Strip` is the output of the strip-builder stage (issue #17): forward
//! priced via put-call parity (§4.2), `k_zero` chosen on the dense grid
//! (§4.5), and `quotes` carrying the Carr-Madan OTM price `Q(K)` and the
//! fitted IV at every dense-grid strike (§4.3 / §4.5).

use serde::{Deserialize, Serialize};

use crate::units::Years;

/// One point on the dense-grid Carr-Madan kernel.
///
/// `q_usd` is the OTM USD option price at `strike`, with the K₀ averaging
/// rule (§4.5) already applied — there is no separate call / put leg at this
/// layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StripQuote {
    pub strike: f64,
    pub q_usd: f64,
    pub iv: f64,
}

/// Per-expiry strip ready for the variance integral (§4.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strip {
    pub forward: f64,
    pub k_zero: f64,
    pub time_to_expiry: Years,
    pub quotes: Vec<StripQuote>,
}
