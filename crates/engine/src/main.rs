//! BVOL index engine entry point.
//!
//! Strip builder (#17), variance integral (#18), 30-day interpolation (#19),
//! and the 60-second scheduler (#20) each land in dedicated issues. This is a
//! stub that prints the methodology version so `cargo run -p volx-engine`
//! succeeds end-to-end from the workspace skeleton.

mod interpolate;
mod strip;
mod variance;

fn main() {
    println!(
        "volx-engine (methodology {})",
        volx_shared_types::METHODOLOGY_VERSION
    );
}
