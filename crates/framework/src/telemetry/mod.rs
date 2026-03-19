//! Native OTLP telemetry: spans, metrics, logs, and context propagation.
//!
//! All telemetry flows through the Rust `tracing` + OpenTelemetry SDK.
//! Python code uses thin PyO3 wrappers — no Python OTEL SDK required.

pub mod config;
pub mod context;
pub mod http;
pub mod logging;
pub mod metrics;
pub mod spans;
pub mod system_metrics;

use pyo3::prelude::*;

/// Check whether performance instrumentation is enabled (`APX_PERF=1`).
///
/// Evaluated once on first call; zero cost thereafter (single atomic load).
/// When enabled, per-phase timing spans are emitted under the `apx.perf`
/// tracing target and flow through the OTEL pipeline.
pub fn perf_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("APX_PERF").is_ok())
}

/// Bootstrap Python-side telemetry: install log handler + init context var.
///
/// Called once during worker startup, after the Python interpreter and
/// event loop are initialized.
pub fn bootstrap_python_telemetry(py: Python<'_>) -> PyResult<()> {
    install_log_handler(py)?;
    context::init_context_var(py)?;
    Ok(())
}

/// Install a Python `logging.Handler` that forwards records to Rust `tracing`.
fn install_log_handler(py: Python<'_>) -> PyResult<()> {
    let emit_fn = pyo3::wrap_pyfunction!(logging::emit_log, py)?;
    let bridge = py.import(c"apx._bridge")?;
    bridge.call_method1(c"install_log_handler", (emit_fn,))?;
    Ok(())
}
