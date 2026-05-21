//! Venue connectors.
//!
//! Each submodule wraps one exchange's WebSocket / REST interface and emits
//! normalised option ticks for downstream consumers. Implementations land in
//! issues #9 (Deribit), #10 (reconnect / backoff), and the post-launch
//! multi-venue milestone (OKX + Bybit).

pub(crate) mod bybit;
pub(crate) mod deribit;
pub(crate) mod okx;
