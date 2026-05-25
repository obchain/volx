//! Time-unit newtypes.
//!
//! Python lets `T` mean years and `N_T` mean minutes interchangeably; the Rust
//! port encodes the unit in the type so a "30-day window" (`Minutes`) is never
//! accidentally fed into `e^{rT}` (`Years`). The constants and conversion
//! factor match `METHODOLOGY.md` §4.6.

use serde::{Deserialize, Serialize};

/// Time expressed in years (the unit `T` uses in `METHODOLOGY.md`).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Years(pub f64);

/// Time expressed in minutes (the unit `N_T`, `N_30`, `N_365` use in CBOE
/// convention, §4.6).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Minutes(pub f64);

impl Minutes {
    /// 30 days expressed in minutes (`N_30` from §4.6).
    pub const N_30D: Self = Self(30.0 * 1440.0);
    /// 365 days expressed in minutes (`N_365` from §4.6).
    pub const N_365D: Self = Self(365.0 * 1440.0);
}

impl Years {
    #[must_use]
    pub fn to_minutes(self) -> Minutes {
        Minutes(self.0 * Minutes::N_365D.0)
    }
}

impl Minutes {
    #[must_use]
    pub fn to_years(self) -> Years {
        Years(self.0 / Self::N_365D.0)
    }
}

impl From<Years> for Minutes {
    fn from(y: Years) -> Self {
        y.to_minutes()
    }
}

impl From<Minutes> for Years {
    fn from(m: Minutes) -> Self {
        m.to_years()
    }
}
