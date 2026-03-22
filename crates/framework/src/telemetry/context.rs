//! Trace context propagation between Rust `tracing` spans and Python.
//!
//! Rust request spans live on tokio threads. Before scheduling Python work
//! on the event loop, we extract the current trace context and inject it
//! into a Python `ContextVar` so that user-created `SpanHandle` instances
//! attach as children.
//!
//! Shared primitives (`SerializedContext`, `read_context_var_raw`,
//! `parse_span_context`) are used by both `spans.rs` and `logging.rs`.

use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
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

// ── ContextVar lifecycle ────────────────────────────────────────────────

/// Initialize the Python `ContextVar` used for trace context propagation.
///
/// Must be called once during worker startup (on the event loop thread).
pub fn init_context_var(py: Python<'_>) -> PyResult<()> {
    let contextvars = py.import(c"contextvars")?;
    let cv = contextvars.call_method1(c"ContextVar", ("_apx_trace_ctx",))?;
    let _ = CONTEXT_VAR.set(cv.unbind());
    tracing::trace!(target: "apx::telemetry", var = "_apx_trace_ctx", "Python trace context ContextVar initialized");
    Ok(())
}

/// Return a reference to the context var (if initialized).
pub fn context_var() -> Option<&'static Py<PyAny>> {
    CONTEXT_VAR.get()
}

// ── Rust tracing → TraceContext ─────────────────────────────────────────

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

// ── Python ContextVar → Rust ────────────────────────────────────────────

/// Hex-encoded trace context as stored in the Python `ContextVar`.
#[derive(Debug, Clone)]
pub(crate) struct SerializedContext {
    pub(crate) trace_id_hex: String,
    pub(crate) span_id_hex: String,
    pub(crate) flags: u8,
}

/// Read the serialized context from the Python `ContextVar`.
pub(crate) fn read_context_var_raw(py: Python<'_>) -> Option<SerializedContext> {
    let cv = context_var()?;
    let val = cv.call_method0(py, c"get").ok()?;
    let (trace_id_hex, span_id_hex, flags) = val.extract::<(String, String, u8)>(py).ok()?;
    Some(SerializedContext {
        trace_id_hex,
        span_id_hex,
        flags,
    })
}

/// Parse hex-encoded trace/span IDs into an OTEL `SpanContext`.
///
/// Returns `None` for any malformed or invalid (all-zeros) identifiers.
/// Invalid contexts must not be attached — in opentelemetry 0.29,
/// `with_remote_span_context` marks the context as active, so an invalid
/// one with `flags=0` would cause `ParentBased` to drop all child spans.
pub(crate) fn parse_span_context(raw: &SerializedContext) -> Option<SpanContext> {
    let tid: [u8; 16] = hex::decode(&raw.trace_id_hex).ok()?.try_into().ok()?;
    let sid: [u8; 8] = hex::decode(&raw.span_id_hex).ok()?.try_into().ok()?;

    let trace_id = TraceId::from_bytes(tid);
    let span_id = SpanId::from_bytes(sid);
    if trace_id == TraceId::INVALID || span_id == SpanId::INVALID {
        return None;
    }

    Some(SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(raw.flags),
        true,
        TraceState::default(),
    ))
}

/// Read the Python ContextVar and parse into an OTEL `SpanContext`.
pub(crate) fn read_python_span_context(py: Python<'_>) -> Option<SpanContext> {
    read_context_var_raw(py)
        .as_ref()
        .and_then(parse_span_context)
}

// ── Rust → Python ContextVar ────────────────────────────────────────────

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
