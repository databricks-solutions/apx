//! Python ASGI boundary layer.
//!
//! Translates Rust domain types (InboundRequest, OutboundResponse) to/from
//! ASGI protocol objects (scope, receive, send).

pub mod app;
pub mod bench_trace;
pub mod dispatch;
pub mod scope;
pub mod streaming;

/// Check whether bench-trace instrumentation is enabled (`APX_BENCH_TRACE=1`).
///
/// Evaluated once on first call; zero cost thereafter (single atomic load).
pub fn bench_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("APX_BENCH_TRACE").is_ok())
}
