//! Forward Python log records into Rust `tracing` events.
//!
//! Python's `logging` hierarchy maps to tracing severity levels.
//! Records flow through: `logging.Handler` → `emit_log()` → `tracing`
//! → `tracing-opentelemetry` → OTLP log exporter.

use pyo3::prelude::*;

/// Python logging level thresholds.
const ERROR: i32 = 40;
const WARNING: i32 = 30;
const INFO: i32 = 20;
const DEBUG: i32 = 10;

/// Forward a single Python log record into the Rust tracing subscriber.
///
/// Python levels map to tracing severity:
/// ERROR+ (40+) → error, WARNING (30) → warn, INFO (20) → info,
/// DEBUG (10) → debug, below → trace.
#[pyfunction]
#[pyo3(name = "_emit_log")]
pub fn emit_log(level: i32, message: String, logger_name: String) {
    match level {
        ERROR.. => tracing::error!(target: "apx::python", logger = logger_name, "{}", message),
        WARNING.. => tracing::warn!(target: "apx::python", logger = logger_name, "{}", message),
        INFO.. => tracing::info!(target: "apx::python", logger = logger_name, "{}", message),
        DEBUG.. => tracing::debug!(target: "apx::python", logger = logger_name, "{}", message),
        _ => tracing::trace!(target: "apx::python", logger = logger_name, "{}", message),
    }
}
