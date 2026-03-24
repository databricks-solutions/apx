//! Forward Python log records into Rust `tracing` events.
//!
//! Python's `logging` hierarchy maps to tracing severity levels.
//! Each record flows through two paths:
//!
//! 1. **Stderr**: `tracing` event with target `apx::python` → fmt layer.
//!    The OTEL log bridge filters this target out (see `tracing_init.rs`).
//! 2. **OTEL export**: direct `LogRecord` via the SDK logger, with trace
//!    context from the Python `ContextVar` when available.
//!
//! The direct OTEL path is necessary because `tracing::Span::current()` is
//! not active on the Python asyncio thread — the `opentelemetry-appender-tracing`
//! bridge would emit log records without trace context.

use opentelemetry::logs::{LogRecord as _, Logger, Severity};
use pyo3::prelude::*;
use std::sync::OnceLock;

/// Python logging level thresholds.
const ERROR: i32 = 40;
const WARNING: i32 = 30;
const INFO: i32 = 20;
const DEBUG: i32 = 10;

/// Cached OTEL logger for direct log record emission.
static OTEL_LOGGER: OnceLock<opentelemetry_sdk::logs::SdkLogger> = OnceLock::new();

fn otel_logger() -> Option<&'static opentelemetry_sdk::logs::SdkLogger> {
    OTEL_LOGGER.get().or_else(|| {
        let provider = apx_core::tracing_init::logger_provider()?;
        let logger = opentelemetry::logs::LoggerProvider::logger(provider, "apx.python");
        let _ = OTEL_LOGGER.set(logger);
        OTEL_LOGGER.get()
    })
}

/// Map Python logging level to OTEL Severity.
fn python_level_to_severity(level: i32) -> Severity {
    match level {
        ERROR.. => Severity::Error,
        WARNING.. => Severity::Warn,
        INFO.. => Severity::Info,
        DEBUG.. => Severity::Debug,
        _ => Severity::Trace,
    }
}

/// Forward a single Python log record into the Rust tracing subscriber.
///
/// Two output paths:
/// 1. `tracing` event (target `apx::python`) → stderr only.
///    The OTEL log bridge filters this target out to avoid duplicates.
/// 2. Direct `LogRecord` to the OTEL log exporter, with trace context
///    from the Python `ContextVar` when available.
#[pyfunction]
#[pyo3(name = "_emit_log")]
pub fn emit_log(py: Python<'_>, level: i32, message: String, logger_name: String) {
    match level {
        ERROR.. => tracing::error!(target: "apx::python", logger = logger_name, "{}", message),
        WARNING.. => tracing::warn!(target: "apx::python", logger = logger_name, "{}", message),
        INFO.. => tracing::info!(target: "apx::python", logger = logger_name, "{}", message),
        DEBUG.. => tracing::debug!(target: "apx::python", logger = logger_name, "{}", message),
        _ => tracing::trace!(target: "apx::python", logger = logger_name, "{}", message),
    }

    let sc = super::context::read_python_span_context(py);
    emit_otel_log_record(sc.as_ref(), level, &message, &logger_name);
}

/// Emit a direct OTEL `LogRecord`, optionally with trace context.
fn emit_otel_log_record(
    sc: Option<&opentelemetry::trace::SpanContext>,
    level: i32,
    message: &str,
    logger_name: &str,
) {
    let Some(logger) = otel_logger() else {
        return;
    };

    let mut record = logger.create_log_record();
    record.set_body(message.to_owned().into());
    record.set_severity_number(python_level_to_severity(level));
    record.set_severity_text(match level {
        ERROR.. => "ERROR",
        WARNING.. => "WARN",
        INFO.. => "INFO",
        DEBUG.. => "DEBUG",
        _ => "TRACE",
    });
    record.add_attribute("logger", logger_name.to_owned());
    if let Some(sc) = sc {
        record.set_trace_context(sc.trace_id(), sc.span_id(), Some(sc.trace_flags()));
    }

    logger.emit(record);
}
