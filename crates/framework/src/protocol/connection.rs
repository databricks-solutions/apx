//! HTTP/1.1 connection handler for asyncio transport/protocol.
//!
//! Implements the asyncio Protocol interface using Rust-accelerated
//! parsing, scope building, and response writing.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use crate::asgi::scope::{ResolvedAwaitableWithValue, ScopeInterns};
use crate::telemetry::dispatch_metrics;
use crate::transport::types::ProtocolVersion;

use super::parser::{HttpVersion, ParsedHead, ParsedRequest, RequestParser};
use super::writer::RustResponseWriter;

/// Maximum concurrent in-flight requests per protocol instance.
const MAX_CONCURRENT: u32 = 256;

/// Seconds of idle time before closing a keep-alive connection.
const KEEPALIVE_TIMEOUT_S: f64 = 5.0;

/// Shared state for all protocol instances on this worker.
struct ProtocolShared {
    on_request: Py<PyAny>,
    interns: ScopeInterns,
    server_host: String,
    server_port: u16,
    active_requests: AtomicU32,
    write_paused: AtomicBool,
}

/// Factory that creates [`RustProtocol`] instances for `loop.create_server()`.
///
/// Holds shared worker state (interns, dispatch callback, concurrency limit).
/// Created in Rust (worker init), passed to Python as a callable.
#[pyclass(module = "apx._core")]
pub struct ProtocolFactory {
    shared: Arc<ProtocolShared>,
}

crate::opaque_debug!(ProtocolFactory);

impl ProtocolFactory {
    /// Create a factory with shared worker state (Rust-side constructor).
    pub fn new(
        on_request: Py<PyAny>,
        interns: ScopeInterns,
        server_host: String,
        server_port: u16,
    ) -> Self {
        Self {
            shared: Arc::new(ProtocolShared {
                on_request,
                interns,
                server_host,
                server_port,
                active_requests: AtomicU32::new(0),
                write_paused: AtomicBool::new(false),
            }),
        }
    }
}

#[pymethods]
impl ProtocolFactory {
    /// Called by asyncio as the protocol factory (`loop.create_server(factory)`).
    fn __call__(&self, py: Python<'_>) -> PyResult<Py<RustProtocol>> {
        Py::new(
            py,
            RustProtocol {
                transport: None,
                parser: RequestParser::new(),
                shared: Arc::clone(&self.shared),
                client_addr: None,
                keepalive_handle: None,
            },
        )
    }
}

/// HTTP/1.1 protocol for asyncio `loop.create_server()`.
///
/// Plugs into asyncio's transport/protocol layer. Parses HTTP
/// requests in Rust and dispatches to a Python callback.
#[pyclass(module = "apx._core")]
pub struct RustProtocol {
    transport: Option<Py<PyAny>>,
    parser: RequestParser,
    shared: Arc<ProtocolShared>,
    client_addr: Option<SocketAddr>,
    keepalive_handle: Option<Py<PyAny>>,
}

crate::opaque_debug!(RustProtocol);

#[pymethods]
impl RustProtocol {
    /// Called by asyncio when a connection is established.
    fn connection_made(&mut self, py: Python<'_>, transport: Py<PyAny>) -> PyResult<()> {
        self.client_addr = extract_peer_addr(py, &transport);
        self.transport = Some(transport);
        dispatch_metrics::inc_connections();
        Ok(())
    }

    /// Close the connection if idle (no active requests).
    ///
    /// Called by the event loop's `call_later` as the keep-alive timeout.
    fn close_idle(&mut self, py: Python<'_>) {
        if self.shared.active_requests.load(Ordering::Relaxed) == 0
            && let Some(transport) = &self.transport
        {
            let _ = transport.call_method0(py, pyo3::intern!(py, "close"));
        }
    }

    /// Called by asyncio when data is received on the connection.
    fn data_received(slf: &Bound<'_, Self>, py: Python<'_>, data: &[u8]) -> PyResult<()> {
        let py_self = slf.clone().unbind();
        let mut this = py_self.borrow_mut(py);
        this.cancel_keepalive_timer(py);
        let t0 = Instant::now();
        let requests = match this.parser.feed(data) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    name: "apx.protocol.parse_error",
                    error = %e,
                    "malformed HTTP request"
                );
                if let Some(transport) = &this.transport {
                    let _ = transport_write(py, transport, REJECT_BAD_REQUEST);
                    let _ = transport.call_method0(py, pyo3::intern!(py, "close"));
                }
                return Ok(());
            }
        };
        dispatch_metrics::record_parse(t0.elapsed().as_micros() as f64);

        let event_loop = py
            .import("asyncio")?
            .call_method0(pyo3::intern!(py, "get_running_loop"))?
            .unbind();

        for parsed in requests {
            // Temporarily drop the borrow so dispatch_request can create
            // a Py<RustProtocol> reference for the OnRequestComplete callback.
            drop(this);
            Self::dispatch_request_inner(py, &py_self, &event_loop, parsed)?;
            this = py_self.borrow_mut(py);
        }
        Ok(())
    }

    /// Called by asyncio when the peer sends EOF.
    #[expect(clippy::unused_self, reason = "required by asyncio protocol interface")]
    fn eof_received(&self) -> bool {
        false
    }

    /// Called by asyncio when the transport's write buffer exceeds
    /// the high-water mark.
    fn pause_writing(&self) {
        self.shared.write_paused.store(true, Ordering::Release);
        tracing::debug!(name: "apx.protocol.pause_writing", "transport write buffer full");
    }

    /// Called by asyncio when the transport's write buffer drains
    /// below the low-water mark.
    fn resume_writing(&self) {
        self.shared.write_paused.store(false, Ordering::Release);
        tracing::debug!(name: "apx.protocol.resume_writing", "transport write buffer drained");
    }

    /// Called by asyncio when the connection is lost.
    fn connection_lost(&mut self, py: Python<'_>, _exc: Option<&Bound<'_, PyAny>>) {
        self.cancel_keepalive_timer(py);
        self.transport = None;
        self.parser.reset();
        dispatch_metrics::dec_connections();
    }
}

impl RustProtocol {
    fn cancel_keepalive_timer(&mut self, py: Python<'_>) {
        if let Some(handle) = self.keepalive_handle.take() {
            let _ = handle.call_method0(py, pyo3::intern!(py, "cancel"));
        }
    }

    fn dispatch_request_inner(
        py: Python<'_>,
        py_self: &Py<Self>,
        event_loop: &Py<PyAny>,
        parsed: ParsedRequest,
    ) -> PyResult<()> {
        let this = py_self.borrow(py);
        let t_dispatch = Instant::now();
        let Some(transport) = &this.transport else {
            return Ok(());
        };

        let active = this.shared.active_requests.fetch_add(1, Ordering::Relaxed);
        if active >= MAX_CONCURRENT {
            this.shared.active_requests.fetch_sub(1, Ordering::Relaxed);
            transport_write(py, transport, REJECT_OVERLOADED)?;
            return Ok(());
        }
        dispatch_metrics::inc_active_requests();
        crate::telemetry::http::inc_active_requests();

        transport.call_method0(py, pyo3::intern!(py, "pause_reading"))?;

        let (request_id, has_request_id) = resolve_request_id(&parsed.head.headers);

        let t_scope = Instant::now();
        let scope = build_scope_from_parsed(
            py,
            &parsed,
            &this.shared.interns,
            &this.shared.server_host,
            this.shared.server_port,
            this.client_addr,
            &request_id,
            has_request_id,
        )?;
        dispatch_metrics::record_scope_build(t_scope.elapsed().as_micros() as f64);

        let t_receive = Instant::now();
        let receive = HttpReceive::new(
            py,
            parsed.body,
            Some(transport.clone_ref(py)),
            parsed.head.expect_continue,
        )?;
        dispatch_metrics::record_receive_build(t_receive.elapsed().as_micros() as f64);

        let method = parsed.head.method.as_str().to_owned();
        let path = parsed.head.path;

        let (request_span, trace_ctx) =
            crate::telemetry::http::begin_request_span(&request_id, &method, &path);
        crate::telemetry::context::set_python_context(py, &trace_ctx)?;

        let transport_clone = transport.clone_ref(py);
        let shared = Arc::clone(&this.shared);
        let on_request = this.shared.on_request.clone_ref(py);
        drop(this);

        let on_complete = OnRequestComplete::create(
            py,
            transport_clone,
            shared,
            t_dispatch,
            method,
            path,
            request_span,
            py_self.clone_ref(py),
            event_loop.clone_ref(py),
        )?;

        let this = py_self.borrow(py);
        let Some(transport) = &this.transport else {
            return Ok(());
        };
        let send =
            RustResponseWriter::new(py, transport.clone_ref(py), Some(on_complete.into_any()))?;
        drop(this);

        on_request.call1(py, (scope, receive, send))?;
        Ok(())
    }
}

/// Callback invoked when a response is fully written.
///
/// Resumes reading on the transport, decrements the active count,
/// records handler_wait duration, emits `http.server.request.duration`,
/// and ends the OTEL request span.
#[pyclass(module = "apx._core")]
struct OnRequestComplete {
    transport: Py<PyAny>,
    shared: Arc<ProtocolShared>,
    dispatch_start: Instant,
    method: String,
    path: String,
    request_span: tracing::Span,
    protocol: Py<RustProtocol>,
    event_loop: Py<PyAny>,
}

crate::opaque_debug!(OnRequestComplete);

impl OnRequestComplete {
    #[expect(
        clippy::too_many_arguments,
        reason = "all fields needed for completion callback; struct builder would add overhead"
    )]
    fn create(
        py: Python<'_>,
        transport: Py<PyAny>,
        shared: Arc<ProtocolShared>,
        dispatch_start: Instant,
        method: String,
        path: String,
        request_span: tracing::Span,
        protocol: Py<RustProtocol>,
        event_loop: Py<PyAny>,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                transport,
                shared,
                dispatch_start,
                method,
                path,
                request_span,
                protocol,
                event_loop,
            },
        )
    }
}

#[pymethods]
impl OnRequestComplete {
    fn __call__(&mut self, py: Python<'_>, status: u16) -> PyResult<()> {
        let elapsed = self.dispatch_start.elapsed();

        {
            let _guard = self.request_span.enter();
            dispatch_metrics::record_handler_wait(elapsed.as_micros() as f64);
            dispatch_metrics::record_dispatch_total(elapsed.as_micros() as f64);

            crate::telemetry::http::record_duration(
                elapsed.as_secs_f64(),
                &self.method,
                "http",
                status,
                &self.path,
                None,
            );

            crate::telemetry::http::finish_request_span(&self.request_span, status);
        }

        // Always decrement counters, even if the transport is gone.
        // If resume_reading fails (connection already closed), we must
        // still release the concurrency slot — otherwise the counter
        // leaks and eventually all requests get 503.
        let resume_result = self
            .transport
            .call_method0(py, pyo3::intern!(py, "resume_reading"));
        self.shared.active_requests.fetch_sub(1, Ordering::Relaxed);
        dispatch_metrics::dec_active_requests();
        crate::telemetry::http::dec_active_requests();

        if let Err(e) = resume_result {
            tracing::debug!(
                name: "apx.protocol.resume_reading_failed",
                error = %e,
                "resume_reading failed (connection likely closed)"
            );
        }

        if let Ok(close_idle) = self.protocol.getattr(py, pyo3::intern!(py, "close_idle"))
            && let Ok(handle) = self.event_loop.call_method1(
                py,
                pyo3::intern!(py, "call_later"),
                (KEEPALIVE_TIMEOUT_S, close_idle),
            )
            && let Ok(mut proto) = self.protocol.try_borrow_mut(py)
        {
            proto.keepalive_handle = Some(handle);
        }

        Ok(())
    }
}

// ── HttpReceive ──────────────────────────────────────────────────────

/// ASGI `receive` callable for non-streaming HTTP requests.
///
/// First call returns the request body immediately via
/// `ResolvedAwaitableWithValue`. Subsequent calls return a pending
/// future (disconnect sentinel). Handles `Expect: 100-continue` by
/// writing the informational response before delivering the body.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct HttpReceive {
    body: std::sync::Mutex<Option<Bytes>>,
    transport: Option<Py<PyAny>>,
    expect_continue: bool,
}

crate::opaque_debug!(HttpReceive);

impl HttpReceive {
    fn new(
        py: Python<'_>,
        body: Bytes,
        transport: Option<Py<PyAny>>,
        expect_continue: bool,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                body: std::sync::Mutex::new(Some(body)),
                transport,
                expect_continue,
            },
        )
    }
}

#[pymethods]
impl HttpReceive {
    fn __call__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let body = self
            .body
            .lock()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
            .take();

        if let Some(b) = body {
            if self.expect_continue
                && let Some(transport) = &self.transport
            {
                let _ = transport_write(py, transport, INFORMATIONAL_CONTINUE);
            }

            let event = PyDict::new(py);
            event.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))?;
            event.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, &b))?;
            event.set_item(pyo3::intern!(py, "more_body"), false)?;
            let awaitable = Py::new(
                py,
                ResolvedAwaitableWithValue::new(event.into_any().unbind()),
            )?;
            Ok(awaitable.into_any())
        } else {
            let fut = py
                .import("asyncio")?
                .call_method0(pyo3::intern!(py, "get_running_loop"))?
                .call_method0(pyo3::intern!(py, "create_future"))?;
            Ok(fut.unbind())
        }
    }
}

// ── Scope building ──────────────────────────────────────────────────────

/// Build an ASGI HTTP scope dict directly from [`ParsedRequest`].
///
/// Bypasses `ScopeSource` trait and `HeaderMap` — works with raw byte
/// pairs from the parser, avoiding the intermediate allocation.
#[expect(
    clippy::too_many_arguments,
    reason = "scope construction needs all ASGI fields; splitting would fragment the hot path"
)]
fn build_scope_from_parsed(
    py: Python<'_>,
    parsed: &ParsedRequest,
    interns: &ScopeInterns,
    server_host: &str,
    server_port: u16,
    client_addr: Option<SocketAddr>,
    request_id: &str,
    has_request_id: bool,
) -> PyResult<Py<PyDict>> {
    let scope = interns
        .scope_template
        .bind(py)
        .call_method0(pyo3::intern!(py, "copy"))?
        .cast_into::<PyDict>()?;

    let version = match parsed.head.version {
        HttpVersion::Http10 => ProtocolVersion::Http10,
        HttpVersion::Http11 => ProtocolVersion::Http11,
    };
    if version != ProtocolVersion::Http11 {
        scope.set_item(
            interns.keys.http_version.bind(py),
            interns.versions.get(py, version),
        )?;
    }

    scope.set_item(interns.keys.method.bind(py), parsed.head.method.as_str())?;
    scope.set_item(
        interns.keys.path.bind(py),
        percent_decode(&parsed.head.path).as_ref(),
    )?;
    scope.set_item(
        interns.keys.raw_path.bind(py),
        PyBytes::new(py, parsed.head.path.as_bytes()),
    )?;
    scope.set_item(
        interns.keys.query_string.bind(py),
        PyBytes::new(py, &parsed.head.query_string),
    )?;

    set_headers_from_parsed(
        py,
        &scope,
        &parsed.head,
        interns,
        request_id,
        has_request_id,
    )?;
    set_addresses(py, &scope, interns, server_host, server_port, client_addr)?;
    scope.set_item(
        interns.keys.path_params.bind(py),
        interns.empty_dict.bind(py),
    )?;
    scope.set_item(interns.keys.state.bind(py), PyDict::new(py))?;

    Ok(scope.unbind())
}

/// Extract existing `x-request-id` from headers or generate a UUID v4.
///
/// Returns `(request_id, has_request_id)` so the scope builder can
/// skip a second header scan.
fn resolve_request_id(headers: &[(Bytes, Bytes)]) -> (String, bool) {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(b"x-request-id")
            && let Ok(s) = std::str::from_utf8(value)
        {
            return (s.to_owned(), true);
        }
    }
    (generate_uuid_v4(), false)
}

/// Set headers list from raw byte pairs (no `HeaderMap` intermediary).
///
/// Prepends `x-request-id` if not already present in the request.
fn set_headers_from_parsed(
    py: Python<'_>,
    scope: &Bound<'_, PyDict>,
    head: &ParsedHead,
    interns: &ScopeInterns,
    request_id: &str,
    has_request_id: bool,
) -> PyResult<()> {
    let extra_cap = usize::from(!has_request_id);
    let mut pairs: Vec<Bound<'_, PyAny>> = Vec::with_capacity(head.headers.len() + extra_cap);

    if !has_request_id {
        let id_name = PyBytes::new(py, b"x-request-id");
        let id_value = PyBytes::new(py, request_id.as_bytes());
        let pair = PyTuple::new(py, [id_name.into_any(), id_value.into_any()])?;
        pairs.push(pair.into_any());
    }

    for (name, value) in &head.headers {
        let n = intern_header_name(py, name, interns);
        let v = PyBytes::new(py, value);
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        pairs.push(pair.into_any());
    }
    let headers_list = PyList::new(py, &pairs)?;
    scope.set_item(interns.keys.headers.bind(py), headers_list)?;
    Ok(())
}

/// Generate a UUID v4 string (random, RFC 4122 variant 1).
fn generate_uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Try to use the header intern cache, falling back to `PyBytes::new`.
fn intern_header_name<'py>(
    py: Python<'py>,
    name: &Bytes,
    interns: &ScopeInterns,
) -> Bound<'py, PyBytes> {
    let name_lower = name.to_ascii_lowercase();
    if let Some(cached) = interns.headers.map.get(name_lower.as_slice()) {
        return cached.bind(py).clone();
    }
    PyBytes::new(py, &name_lower)
}

/// Set server and client address tuples.
fn set_addresses(
    py: Python<'_>,
    scope: &Bound<'_, PyDict>,
    interns: &ScopeInterns,
    _server_host: &str,
    _server_port: u16,
    client_addr: Option<SocketAddr>,
) -> PyResult<()> {
    scope.set_item(interns.keys.server.bind(py), interns.server_tuple.bind(py))?;
    match client_addr {
        Some(addr) => {
            scope.set_item(
                interns.keys.client.bind(py),
                (addr.ip().to_string(), addr.port()),
            )?;
        }
        None => scope.set_item(interns.keys.client.bind(py), py.None())?,
    }
    Ok(())
}

/// Percent-decode a URL path.
fn percent_decode(input: &str) -> Cow<'_, str> {
    percent_encoding::percent_decode_str(input).decode_utf8_lossy()
}

/// Extract the peer address from an asyncio transport.
fn extract_peer_addr(py: Python<'_>, transport: &Py<PyAny>) -> Option<SocketAddr> {
    let peername = transport
        .call_method1(
            py,
            pyo3::intern!(py, "get_extra_info"),
            (pyo3::intern!(py, "peername"),),
        )
        .ok()?;
    if peername.is_none(py) {
        return None;
    }
    let bound = peername.bind(py);
    let tuple: &Bound<'_, PyTuple> = bound.cast().ok()?;
    let host: String = tuple.get_item(0).ok()?.extract().ok()?;
    let port: u16 = tuple.get_item(1).ok()?.extract().ok()?;
    let ip: std::net::IpAddr = host.parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

// ── Pre-built error responses (sans-I/O: pure data, no transport) ───

/// Sent when the parser cannot decode the incoming bytes as valid HTTP.
const REJECT_BAD_REQUEST: &[u8] = b"HTTP/1.1 400 Bad Request\r\n\
    content-length: 11\r\n\
    content-type: text/plain\r\n\
    connection: close\r\n\
    \r\n\
    Bad Request";

/// Sent when the per-connection concurrency limit is reached.
const REJECT_OVERLOADED: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
    content-length: 19\r\n\
    content-type: text/plain\r\n\
    \r\n\
    Service Unavailable";

/// Informational response for `Expect: 100-continue`.
const INFORMATIONAL_CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";

// ── Transport I/O helper ────────────────────────────────────────────

/// Write raw bytes to an asyncio transport.
fn transport_write(py: Python<'_>, transport: &Py<PyAny>, data: &[u8]) -> PyResult<()> {
    let py_bytes = PyBytes::new(py, data);
    transport.call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))?;
    Ok(())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_generate_uuid_v4_format() {
        let id = generate_uuid_v4();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
    }

    #[test]
    fn test_generate_uuid_v4_version_bits() {
        let id = generate_uuid_v4();
        let version_char = id.chars().nth(14).expect("version char");
        assert_eq!(version_char, '4', "UUID version nibble should be 4");
    }

    #[test]
    fn test_generate_uuid_v4_variant_bits() {
        let id = generate_uuid_v4();
        let variant_char = id.chars().nth(19).expect("variant char");
        assert!(
            matches!(variant_char, '8' | '9' | 'a' | 'b'),
            "UUID variant nibble should be 8/9/a/b, got {variant_char}"
        );
    }

    #[test]
    fn test_generate_uuid_v4_uniqueness() {
        let a = generate_uuid_v4();
        let b = generate_uuid_v4();
        assert_ne!(a, b);
    }
}
