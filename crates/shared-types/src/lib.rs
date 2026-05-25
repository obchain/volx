//! Core domain types shared by the ingestion, normalizer, and engine crates.
//!
//! Everything in this crate is **pure data** — no I/O, no math beyond unit
//! conversions, no venue knowledge. The methodology contract lives in the
//! root `METHODOLOGY.md`; type docs cite the sections they implement.

pub mod ids;
pub mod index;
pub mod strip;
pub mod tick;
pub mod units;

pub use ids::{Asset, IndexId, OptionKind, Venue};
pub use index::{IndexValue, StripHash};
pub use strip::{Strip, StripQuote};
pub use tick::OptionTick;
pub use units::{Minutes, Years};

/// Methodology version this build targets — must match the
/// `## 10. Change log` entry in the root `METHODOLOGY.md`.
pub const METHODOLOGY_VERSION: &str = "0.1.0";
