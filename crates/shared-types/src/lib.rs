//! Core domain types shared by the ingestion, normalizer, and engine crates.
//!
//! Concrete type definitions (`OptionTick`, `Strip`, `IndexValue`, venue / side
//! enums, etc.) land in issue #8. This crate currently exposes only a stable
//! crate name so downstream crates can depend on it from PR #7 onwards.

/// Methodology version this build targets — must match the
/// `## 10. Change log` entry in the root `METHODOLOGY.md`.
pub const METHODOLOGY_VERSION: &str = "0.1.0";
