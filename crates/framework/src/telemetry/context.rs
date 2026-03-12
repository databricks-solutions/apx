//! Trace context propagation between Rust `tracing` spans and Python.
//!
//! Rust request spans live on tokio threads. Before scheduling Python work
//! on the event loop, we extract the current trace context and inject it
//! into a Python `ContextVar` so that user-created `SpanHandle` instances
//! attach as children.

use pyo3::prelude::*;
use std::sync::OnceLock;

/// Cached reference to the Python `ContextVar` for trace context.
static CONTEXT_VAR: OnceLock<Py<PyAny>> = OnceLock::new();

/// Trace identity extracted from a Rust `tracing::Span`.
#[derive(Debug, Clone, Copy)]
pub struct TraceContext {
    /// 16-byte trace identifier.
    pub trace_id: [u8; 16],
    /// 8-byte span identifier.
    pub span_id: [u8; 8],
    /// W3C trace flags.
    pub trace_flags: u8,
}

/// Initialize the Python `ContextVar` used for trace context propagation.
///
/// Must be called once during worker startup (on the event loop thread).
pub fn init_context_var(py: Python<'_>) -> PyResult<()> {
    let contextvars = py.import(c"contextvars")?;
    let cv = contextvars.call_method1(c"ContextVar", ("_apx_trace_ctx",))?;
    let _ = CONTEXT_VAR.set(cv.unbind());
    Ok(())
}

/// Return a reference to the context var (if initialized).
pub fn context_var() -> Option<&'static Py<PyAny>> {
    CONTEXT_VAR.get()
}

/// Extract trace context from the active `tracing::Span` via `tracing-opentelemetry`.
pub fn extract_trace_context() -> Option<TraceContext> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::Span::current();
    let cx = span.context();
    let sc = cx.span().span_context().clone();
    if !sc.is_valid() {
        return None;
    }
    Some(TraceContext {
        trace_id: sc.trace_id().to_bytes(),
        span_id: sc.span_id().to_bytes(),
        trace_flags: sc.trace_flags().to_u8(),
    })
}

/// Push trace context into the Python `ContextVar` so `SpanHandle` picks it up.
///
/// Creates a tuple `(trace_id_hex, span_id_hex, trace_flags)` in the context var.
/// Called on the event loop thread before invoking the Python handler.
pub fn set_python_context(py: Python<'_>, ctx: &TraceContext) -> PyResult<()> {
    let Some(cv) = context_var() else {
        return Ok(());
    };
    let trace_id_hex = hex::encode(ctx.trace_id);
    let span_id_hex = hex::encode(ctx.span_id);
    let value = (trace_id_hex, span_id_hex, ctx.trace_flags);
    cv.call_method1(py, c"set", (value,))?;
    Ok(())
}
