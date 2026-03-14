//! HTTP ↔ Python ASGI bridge.

pub mod asgi;
pub mod asgi_dispatch;
pub mod streaming;

/// Check whether bench-trace instrumentation is enabled (`APX_BENCH_TRACE=1`).
///
/// Evaluated once on first call; zero cost thereafter (single atomic load).
pub fn bench_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("APX_BENCH_TRACE").is_ok())
}
