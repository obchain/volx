//! BVOL index engine entry point.
//!
//! The 60-second scheduler that ties the strip builder (#17), variance
//! integral (#18), and 30-day interpolation (#19) into a live publisher
//! lands in issue #20. Until then this binary just prints the
//! methodology version so `cargo run -p volx-engine` succeeds end-to-end.
//!
//! All numerics live in the library crate (`crates/engine/src/lib.rs`)
//! so tests + future callers can drive the stages without a binary
//! wrapper.

fn main() {
    println!(
        "volx-engine (methodology {})",
        volx_shared_types::METHODOLOGY_VERSION
    );
}
