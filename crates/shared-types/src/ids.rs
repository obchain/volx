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

/// Underlying crypto asset for the option contract. BVOL today is BTC + ETH;
/// any new asset bumps `METHODOLOGY_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Asset {
    Btc,
    Eth,
}

/// Option leg side (call or put).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    Call,
    Put,
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
