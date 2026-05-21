//! Quote filters + tick deduplication.
//!
//! Filter implementations (staleness, crossed/locked, spread cap, intrinsic
//! filter — see `METHODOLOGY.md` §3.1) land in issues #12–#13. This crate
//! currently exposes only a stable crate name so the engine can depend on it.

// Re-export the type crate so downstream consumers reach domain types via the
// normalizer when they want the post-filter forms.
pub use volx_shared_types as types;
