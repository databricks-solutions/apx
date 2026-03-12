//! Native OTLP telemetry: spans, metrics, logs, and context propagation.
//!
//! All telemetry flows through the Rust `tracing` + OpenTelemetry SDK.
//! Python code uses thin PyO3 wrappers — no Python OTEL SDK required.

pub mod context;
pub mod logging;
pub mod metrics;
pub mod spans;

use pyo3::prelude::*;

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
    let code = c"
import logging

class _ApxHandler(logging.Handler):
    def __init__(self, emit_fn):
        super().__init__()
        self._emit = emit_fn

    def emit(self, record):
        try:
            self._emit(record.levelno, record.getMessage(), record.name)
        except Exception:
            pass

def _install(emit_fn):
    handler = _ApxHandler(emit_fn)
    logging.root.addHandler(handler)
";
    let globals = pyo3::types::PyDict::new(py);
    py.run(code, Some(&globals), None)?;
    let install_fn = globals
        .get_item("_install")?
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("_install not found"))?;
    install_fn.call1((emit_fn,))?;
    Ok(())
}
