//! Quote filters + tick deduplication.
//!
//! Filter implementations (staleness, crossed/locked, spread cap, intrinsic
//! filter — see `METHODOLOGY.md` §3.1) land in issues #12–#13. This crate
//! currently exposes only a stable crate name so the engine can depend on it.
