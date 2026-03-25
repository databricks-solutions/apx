//! `SpanHandle` — a PyO3 class wrapping an OpenTelemetry span.
//!
//! Usable as both sync and async context manager from Python:
//!
//! ```python
//! with SpanHandle("db.query", {"table": "users"}):
//!     ...
//!
//! async with SpanHandle("fetch_data"):
//!     ...
//! ```

use opentelemetry::trace::{SpanContext, SpanKind, TraceContextExt, Tracer};
use opentelemetry::{Context, trace::Span};
use pyo3::prelude::*;
use std::collections::HashMap;

use super::context::SerializedContext;

/// Cached tracer for user-created spans, with full InstrumentationScope.
static USER_TRACER: std::sync::OnceLock<opentelemetry::global::BoxedTracer> =
    std::sync::OnceLock::new();

/// Obtain a tracer for user spans with version + schema_url on the scope.
fn user_tracer() -> &'static opentelemetry::global::BoxedTracer {
    USER_TRACER.get_or_init(|| {
        let scope = opentelemetry::InstrumentationScope::builder("apx.user")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_schema_url(super::SEMCONV_SCHEMA_URL)
            .build();
        opentelemetry::global::tracer_with_scope(scope)
    })
}

/// Map Python SpanKind integer to OTEL SpanKind.
fn map_span_kind(kind: u8) -> SpanKind {
    match kind {
        2 => SpanKind::Server,
        3 => SpanKind::Client,
        4 => SpanKind::Producer,
        5 => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}

/// OTEL span status code exposed as a Python enum.
#[pyclass(module = "apx._core", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    /// The span completed successfully.
    Ok = 0,
    /// The span recorded an error.
    Error = 1,
}

/// Python-visible span wrapper backed by OpenTelemetry.
#[pyclass(module = "apx._core")]
pub struct SpanHandle {
    /// Span name.
    name: String,
    /// User-provided attributes to set on the span.
    attributes: HashMap<String, String>,
    /// OTLP SpanKind (1=INTERNAL, 2=SERVER, 3=CLIENT, 4=PRODUCER, 5=CONSUMER).
    kind: u8,
    /// The active OTEL span — `Some` between `__enter__` and `__exit__`.
    span: Option<opentelemetry::global::BoxedSpan>,
    /// Serialized parent context to restore on `__exit__`.
    saved_parent: Option<SerializedContext>,
}

impl std::fmt::Debug for SpanHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpanHandle")
            .field("name", &self.name)
            .field("active", &self.span.is_some())
            .finish()
    }
}

#[cfg(test)]
impl SpanHandle {
    /// Rust-only constructor for integration tests.
    pub(crate) fn create(name: String, attributes: Option<HashMap<String, String>>) -> Self {
        Self {
            name,
            attributes: attributes.unwrap_or_default(),
            kind: 1,
            span: None,
            saved_parent: None,
        }
    }
}

#[pymethods]
impl SpanHandle {
    /// Create a new span handle. The span starts on `__enter__`.
    #[new]
    #[pyo3(signature = (name, attributes=None, kind=1))]
    fn new(name: String, attributes: Option<HashMap<String, String>>, kind: u8) -> Self {
        Self {
            name,
            attributes: attributes.unwrap_or_default(),
            kind,
            span: None,
            saved_parent: None,
        }
    }

    /// Start the span as a child of the current Python trace context.
    fn __enter__(mut slf: PyRefMut<'_, Self>) -> Py<Self> {
        static FIRST: std::sync::Once = std::sync::Once::new();

        let py = slf.py();
        let parent_cx = resolve_parent_context(py);

        // Save current context var value so we can restore it in __exit__.
        slf.saved_parent = super::context::read_context_var_raw(py);

        let tracer = user_tracer();
        let mut span = tracer
            .span_builder(slf.name.clone())
            .with_kind(map_span_kind(slf.kind))
            .start_with_context(tracer, &parent_cx);

        // Apply user-provided attributes.
        for (k, v) in &slf.attributes {
            span.set_attribute(opentelemetry::KeyValue::new(k.clone(), v.clone()));
        }

        // Push this span's context into the Python ContextVar.
        let sc = span.span_context().clone();
        write_context_var(py, &sc);

        FIRST.call_once(|| {
            let has_provider = apx_core::tracing_init::tracer_provider().is_some();
            tracing::info!(
                name: "apx.telemetry.first_user_span",
                target: "apx::telemetry",
                span_name = slf.name.as_str(),
                trace_id = %hex::encode(sc.trace_id().to_bytes()),
                tracer_provider = has_provider,
                "spans: first user span created"
            );
        });

        slf.span = Some(span);
        slf.into()
    }

    /// End the span, restore previous context.
    ///
    /// If the block raised an exception the span is automatically marked
    /// as errored and the traceback is recorded as an event.
    #[pyo3(signature = (exc_type=None, exc_val=None, exc_tb=None))]
    fn __exit__(
        &mut self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        exc_val: Option<&Bound<'_, PyAny>>,
        exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        if let Some(ref mut span) = self.span
            && let Some(exc) = exc_val
        {
            record_python_exception(span, exc_type, exc, exc_tb);
        }
        if let Some(mut span) = self.span.take() {
            span.end();
        }
        restore_context_var(py, self.saved_parent.take());
        false
    }

    /// Async context manager entry — delegates to sync (ContextVars are async-safe).
    fn __aenter__(slf: PyRefMut<'_, Self>) -> PyResult<Bound<'_, PyAny>> {
        let py = slf.py();
        let entered = Self::__enter__(slf);
        resolved_coroutine(py, entered.into_any())
    }

    /// Async context manager exit.
    #[pyo3(signature = (exc_type=None, exc_val=None, exc_tb=None))]
    fn __aexit__<'py>(
        &mut self,
        py: Python<'py>,
        exc_type: Option<&Bound<'_, PyAny>>,
        exc_val: Option<&Bound<'_, PyAny>>,
        exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.__exit__(py, exc_type, exc_val, exc_tb);
        let py_false = false.into_pyobject(py)?.to_owned().into_any().unbind();
        resolved_coroutine(py, py_false)
    }

    /// Add an event to the current span.
    #[pyo3(signature = (name, attributes=None))]
    fn add_event(&mut self, name: String, attributes: Option<HashMap<String, String>>) {
        if let Some(ref mut span) = self.span {
            let attrs: Vec<opentelemetry::KeyValue> = attributes
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| opentelemetry::KeyValue::new(k, v))
                .collect();
            span.add_event(name, attrs);
        }
    }

    /// Set an attribute on the span.
    fn set_attribute(&mut self, key: String, value: String) {
        if let Some(ref mut span) = self.span {
            span.set_attribute(opentelemetry::KeyValue::new(key, value));
        }
    }

    /// Set the span status.
    #[pyo3(signature = (code, description=""))]
    fn set_status(&mut self, code: StatusCode, description: &str) {
        let Some(ref mut span) = self.span else {
            return;
        };
        let status = match code {
            StatusCode::Error => opentelemetry::trace::Status::error(description.to_owned()),
            StatusCode::Ok => opentelemetry::trace::Status::Ok,
        };
        span.set_status(status);
    }

    /// Record an exception as a span event with OTEL semantic attributes.
    #[pyo3(signature = (message, type_name="Exception", stacktrace=""))]
    fn record_exception(&mut self, message: &str, type_name: &str, stacktrace: &str) {
        let Some(ref mut span) = self.span else {
            return;
        };
        let mut attrs = vec![
            opentelemetry::KeyValue::new("exception.type", type_name.to_owned()),
            opentelemetry::KeyValue::new("exception.message", message.to_owned()),
        ];
        if !stacktrace.is_empty() {
            attrs.push(opentelemetry::KeyValue::new(
                "exception.stacktrace",
                stacktrace.to_owned(),
            ));
        }
        span.add_event("exception", attrs);
    }
}

// ── Exception capture ────────────────────────────────────────────────

/// Record a Python exception on the span: add an ``exception`` event and
/// set the span status to Error.
fn record_python_exception(
    span: &mut opentelemetry::global::BoxedSpan,
    exc_type: Option<&Bound<'_, PyAny>>,
    exc_val: &Bound<'_, PyAny>,
    exc_tb: Option<&Bound<'_, PyAny>>,
) {
    let type_name = exc_type
        .and_then(|t| t.getattr("__qualname__").ok())
        .and_then(|n| n.extract::<String>().ok())
        .unwrap_or_else(|| "Exception".to_owned());

    let message = exc_val.str().map(|s| s.to_string()).unwrap_or_default();

    let stacktrace = exc_tb
        .and_then(|tb| format_traceback(tb).ok())
        .unwrap_or_default();

    let mut attrs = vec![
        opentelemetry::KeyValue::new("exception.type", type_name),
        opentelemetry::KeyValue::new("exception.message", message.clone()),
    ];
    if !stacktrace.is_empty() {
        attrs.push(opentelemetry::KeyValue::new(
            "exception.stacktrace",
            stacktrace,
        ));
    }
    span.add_event("exception", attrs);
    span.set_status(opentelemetry::trace::Status::error(message));
}

/// Format a Python traceback object into a string via ``traceback.format_tb``.
fn format_traceback(tb: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = tb.py();
    let tb_mod = py.import(pyo3::intern!(py, "traceback"))?;
    let lines: Vec<String> = tb_mod
        .call_method1(pyo3::intern!(py, "format_tb"), (tb,))?
        .extract()?;
    Ok(lines.join(""))
}

// ── Serialized trace context ──────────────────────────────────────────

/// Invalid/empty trace context written to the ContextVar when no parent exists.
const EMPTY_TRACE_ID_HEX: &str = "00000000000000000000000000000000";
const EMPTY_SPAN_ID_HEX: &str = "0000000000000000";

// ── Context var helpers ─────────────────────────────────────────────────

/// Build an OTEL `Context` from the Python ContextVar.
fn resolve_parent_context(py: Python<'_>) -> Context {
    super::context::read_python_span_context(py)
        .map(|sc| Context::new().with_remote_span_context(sc))
        .unwrap_or_default()
}

/// Write a `SpanContext` into the Python ContextVar.
fn write_context_var(py: Python<'_>, sc: &SpanContext) {
    let Some(cv) = super::context::context_var() else {
        return;
    };
    let value = (
        hex::encode(sc.trace_id().to_bytes()),
        hex::encode(sc.span_id().to_bytes()),
        sc.trace_flags().to_u8(),
        sc.trace_state().header(),
    );
    let _ = cv.call_method1(py, c"set", (value,));
}

/// Restore the ContextVar to a previous value (or clear it).
fn restore_context_var(py: Python<'_>, saved: Option<SerializedContext>) {
    let Some(cv) = super::context::context_var() else {
        return;
    };
    let tuple = match saved {
        Some(ctx) => (
            ctx.trace_id_hex,
            ctx.span_id_hex,
            ctx.flags,
            ctx.trace_state,
        ),
        None => (
            EMPTY_TRACE_ID_HEX.to_owned(),
            EMPTY_SPAN_ID_HEX.to_owned(),
            0u8,
            String::new(),
        ),
    };
    let _ = cv.call_method1(py, c"set", (tuple,));
}

/// Cached Python function that builds an immediately-resolved coroutine.
static RESOLVED_FN: std::sync::OnceLock<Py<PyAny>> = std::sync::OnceLock::new();

/// Import and cache the `resolved` async function from `apx._bridge`.
fn init_resolved_fn(py: Python<'_>) -> PyResult<&'static Py<PyAny>> {
    if let Some(f) = RESOLVED_FN.get() {
        return Ok(f);
    }
    let bridge = py.import(c"apx._bridge")?;
    let f = bridge.getattr(c"resolved")?.unbind();
    let _ = RESOLVED_FN.set(f);
    RESOLVED_FN
        .get()
        .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("failed to cache resolved"))
}

/// Build a Python coroutine that immediately resolves to the given value.
fn resolved_coroutine(py: Python<'_>, value: Py<PyAny>) -> PyResult<Bound<'_, PyAny>> {
    let resolved_fn = init_resolved_fn(py)?;
    resolved_fn.call1(py, (value,)).map(|v| v.into_bound(py))
}
