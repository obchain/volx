//! `VolX` BVOL/EVOL index engine.
//!
//! Single-character bindings (`f`, `k`, `t`, `r`, `iv`, `c`, `p`) follow
//! the canonical Black-Scholes / CBOE-VIX notation used in
//! `METHODOLOGY.md`; rename-them-all clippy lints (`many_single_char_names`,
//! `similar_names`) fight the math more than they help.
#![allow(clippy::many_single_char_names, clippy::similar_names)]
//!
//! The engine sits between the normalizer's tick stream (#16) and the
//! published index (#20). Pipeline stages, by issue:
//!
//! - **#17 — strip builder.** Per-expiry forward via put-call parity
//!   (§4.2), fitted-IV cubic spline (§4.3), Black-Scholes Carr-Madan
//!   OTM prices on a 801-point dense grid (§4.5). See [`strip`].
//! - **#18 — variance integral.** Trapezoidal `σ²_T` over the dense
//!   grid (§4.5).
//! - **#19 — 30-day interpolation.** Total-variance interpolation
//!   between near + next expiries (§4.6).
//! - **#20 — scheduler.** 60-second cadence, `ClickHouse` + Redis sinks.
//!
//! This crate exposes the numerics as a library so the binary entry
//! point in `main.rs` (and unit tests) can drive them. Public surface
//! is deliberately narrow — `OptionTick` and `Strip` come from
//! `volx-shared-types`; the engine just adds the per-stage builders.

pub mod bs;
pub mod interpolate;
pub mod spline;
pub mod strip;
pub mod variance;

pub use interpolate::{ExpiryVariance, InterpError, bvol, interpolate_30d};
pub use strip::{
    BuildError, ChainLeg, DENSE_GRID_POINTS, ExpiryChain, build_strip, build_strip_with_rate,
};
pub use variance::{VarianceError, variance_t, variance_t_with_rate};
