//! Categorical identifiers — venues, assets, option sides, published indices.
//!
//! Snake-case JSON for the operational enums matches Deribit / OKX / Bybit
//! wire formats; the published index id is uppercase to match the public
//! ticker convention in `conventions.md`.

use serde::{Deserialize, Serialize};

/// Trading venue a tick came from. Add a variant + a venues module under
/// `crates/ingestion/src/venues/` together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    Deribit,
    Okx,
    Bybit,
}

impl Venue {
    /// Canonical lowercase label. Single source of truth for:
    /// - Prometheus label value (`{venue="deribit"}`)
    /// - Normalizer Redis topic segment (`options:{venue}:...`)
    /// - `ClickHouse` `options_ticks.venue` column value
    ///
    /// Dashboards + downstream queries key on these strings — do not
    /// rename without a wire-format bump.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Deribit => "deribit",
            Self::Okx => "okx",
            Self::Bybit => "bybit",
        }
    }
}

/// Underlying crypto asset for the option contract. BVOL today is BTC + ETH;
/// any new asset bumps `METHODOLOGY_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Asset {
    Btc,
    Eth,
}

impl Asset {
    /// Canonical lowercase label. Same stability contract as
    /// [`Venue::label`] — keep in sync with the `serde(rename_all =
    /// "snake_case")` wire format on this enum.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Btc => "btc",
            Self::Eth => "eth",
        }
    }
}

/// Option leg side (call or put).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    Call,
    Put,
}

impl OptionKind {
    /// Canonical lowercase label. Same stability contract as
    /// [`Venue::label`].
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Put => "put",
        }
    }
}

/// Published index ticker. The 4-letter uppercase form is the public symbol
/// (`BVOL` for BTC, `EVOL` for ETH); per `conventions.md` index IDs are
/// uppercase 4-letter strings, so serde renames lock the wire format
/// regardless of how Rust names the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IndexId {
    #[serde(rename = "BVOL")]
    Bvol,
    #[serde(rename = "EVOL")]
    Evol,
}

impl IndexId {
    /// Public ticker for this index, e.g. `"BVOL"`.
    #[must_use]
    pub const fn ticker(self) -> &'static str {
        match self {
            Self::Bvol => "BVOL",
            Self::Evol => "EVOL",
        }
    }

    /// Underlying asset this index tracks.
    #[must_use]
    pub const fn asset(self) -> Asset {
        match self {
            Self::Bvol => Asset::Btc,
            Self::Evol => Asset::Eth,
        }
    }
}
