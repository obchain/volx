//! Per-option market tick.
//!
//! `OptionTick` is the canonical normalized representation of a single
//! `(venue, asset, expiry, strike, kind)` quote after the venue-specific
//! decoder runs but before any aggregation. The normalizer crate operates
//! on these; the engine consumes synchronized snapshots built from them.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{Asset, OptionKind, Venue};

/// Single normalized option quote.
///
/// Field conventions (see `METHODOLOGY.md` §2):
/// - `strike` and `underlying` are in **USD**.
/// - `bid` / `ask` / `mid` are **USD** prices already converted from
///   coin-quoted venue prices (`price_usd = price_coin × underlying`, §2.1).
/// - `iv` is the venue-published implied volatility expressed as a **decimal
///   fraction** (e.g. `0.65`, not `65.0`). Per-venue percent→fraction
///   normalization happens in the ingestion decoder, not here.
/// - `bid`, `ask`, `mid`, `iv` are `Option<f64>` because the normalizer's
///   per-side filters (§3.1) may invalidate one side of a quote independently.
/// - `expiry` and `received_at` are UTC, millisecond precision (§5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionTick {
    pub venue: Venue,
    pub asset: Asset,
    #[serde(with = "time::serde::rfc3339")]
    pub expiry: OffsetDateTime,
    pub strike: f64,
    pub kind: OptionKind,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub mid: Option<f64>,
    pub iv: Option<f64>,
    pub underlying: f64,
    pub open_interest: f64,
    pub volume_24h: f64,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
}
