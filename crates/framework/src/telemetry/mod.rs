//! Native OTLP telemetry: spans, metrics, logs, and context propagation.
//!
//! All telemetry flows through the Rust `tracing` + OpenTelemetry SDK.
//! Python code uses thin PyO3 wrappers — no Python OTEL SDK required.

pub mod config;
pub mod context;
pub mod defs;
pub mod dispatch_metrics;
pub mod http;
pub mod logging;
pub mod metrics;
pub mod process_metrics;
pub mod spans;
pub mod system_metrics;

use opentelemetry::metrics::MeterProvider;
use pyo3::prelude::*;

/// Obtain a named OTEL meter backed by the configured provider.
///
/// Falls back to the global (noop when OTEL is disabled — zero overhead).
pub(crate) fn get_meter(name: &'static str) -> opentelemetry::metrics::Meter {
    if let Some(mp) = apx_core::tracing_init::meter_provider() {
        mp.meter(name)
    } else {
        opentelemetry::global::meter(name)
    }
}

/// Generate a module-local toggle store: `static` + `pub fn init()` + `fn toggles()`.
///
/// Eliminates the repeated `OnceLock + init + accessor` boilerplate for
/// metric toggle structs that are initialized once per worker process.
macro_rules! toggle_store {
    ($static_name:ident : $ty:ty = $default:expr) => {
        static $static_name: std::sync::OnceLock<$ty> = std::sync::OnceLock::new();

        /// Initialize toggles for this process. Subsequent calls are ignored.
        pub fn init(toggles: $ty) {
            let _ = $static_name.set(toggles);
        }

        /// Return active toggles, falling back to compile-time defaults.
        fn toggles() -> &'static $ty {
            static DEFAULT: $ty = $default;
            $static_name.get().unwrap_or(&DEFAULT)
        }
    };
}
pub(crate) use toggle_store;

/// Bootstrap Python-side telemetry: install log handler + init context var.
///
/// Called once during worker startup, after the Python interpreter and
/// event loop are initialized.
pub fn bootstrap_python_telemetry(py: Python<'_>) -> PyResult<()> {
    tracing::trace!(target: "apx::telemetry", "bootstrapping Python-side telemetry");
    install_log_handler(py)?;
    context::init_context_var(py)?;
    tracing::trace!(target: "apx::telemetry", "Python telemetry bootstrap complete");
    Ok(())
}

/// Install a Python `logging.Handler` that forwards records to Rust `tracing`.
fn install_log_handler(py: Python<'_>) -> PyResult<()> {
    let emit_fn = pyo3::wrap_pyfunction!(logging::emit_log, py)?;
    let bridge = py.import(c"apx._bridge")?;
    bridge.call_method1(c"install_log_handler", (emit_fn,))?;
    tracing::trace!(target: "apx::telemetry", "Python log handler installed (apx._bridge)");
    Ok(())
}
