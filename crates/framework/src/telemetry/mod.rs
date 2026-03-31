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

use opentelemetry::InstrumentationScope;
use opentelemetry::metrics::MeterProvider;
use pyo3::prelude::*;

/// OpenTelemetry semantic conventions version this instrumentation conforms to.
const SEMCONV_SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.29.0";

/// Obtain a named OTEL meter backed by the configured provider.
///
/// Attaches the framework version and OTEL semconv schema URL to the
/// instrumentation scope so that `metric_schema_url` is populated in
/// exported telemetry.
pub(crate) fn get_meter(name: &'static str) -> opentelemetry::metrics::Meter {
    let scope = InstrumentationScope::builder(name)
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_schema_url(SEMCONV_SCHEMA_URL)
        .build();

    if let Some(mp) = apx_core::tracing_init::meter_provider() {
        mp.meter_with_scope(scope)
    } else {
        opentelemetry::global::meter_with_scope(scope)
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

/// Time an expression and record elapsed microseconds via a metric function.
///
/// Rust equivalent of Python's `with timing(metric): <ops>`.
/// Works with `?`, `.await`, blocks, and nested `timed!` calls.
macro_rules! timed {
    ($record:path, $expr:expr) => {{
        let __t0 = ::std::time::Instant::now();
        let __val = $expr;
        $record(__t0.elapsed().as_micros() as f64);
        __val
    }};
}
pub(crate) use timed;

/// State that can be refreshed before reading.
///
/// Implemented by `SystemState` and `ProcessState` to share the
/// lock-refresh-read pattern across observable metric callbacks.
pub(super) trait Refreshable {
    fn ensure_fresh(&mut self);
}

/// Lock a shared `Refreshable`, refresh if stale, then invoke `f`.
pub(super) fn with_fresh<S, F>(state: &std::sync::Arc<std::sync::Mutex<S>>, f: F)
where
    S: Refreshable,
    F: FnOnce(&S),
{
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.ensure_fresh();
    f(&s);
}

/// Bootstrap Python-side telemetry: install log handler + init context var.
///
/// Called once during worker startup, after the Python interpreter and
/// event loop are initialized.
pub fn bootstrap_python_telemetry(py: Python<'_>) -> PyResult<()> {
    tracing::trace!(name: "apx.telemetry.bootstrap_start", target: "apx::telemetry", "bootstrapping Python-side telemetry");
    install_log_handler(py)?;
    context::init_context_var(py)?;
    tracing::trace!(name: "apx.telemetry.bootstrap_complete", target: "apx::telemetry", "Python telemetry bootstrap complete");
    Ok(())
}

/// Install a Python `logging.Handler` that forwards records to Rust `tracing`.
fn install_log_handler(py: Python<'_>) -> PyResult<()> {
    let emit_fn = pyo3::wrap_pyfunction!(logging::emit_log, py)?;
    let bridge = py.import(c"apx._bridge")?;
    bridge.call_method1(c"install_log_handler", (emit_fn,))?;
    tracing::trace!(name: "apx.telemetry.log_handler_installed", target: "apx::telemetry", "Python log handler installed (apx._bridge)");
    Ok(())
}
