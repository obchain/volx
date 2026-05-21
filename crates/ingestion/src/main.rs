//! Ingestion entry point.
//!
//! Deribit / OKX / Bybit WebSocket connectors land in issues #9–#11.
//! This is currently a stub that prints the methodology version it would
//! ingest data for, so `cargo run -p volx-ingestion` succeeds end-to-end
//! from the workspace skeleton.

mod venues;

fn main() {
    println!(
        "volx-ingestion (methodology {})",
        volx_shared_types::METHODOLOGY_VERSION
    );
}
