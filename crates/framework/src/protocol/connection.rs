//! HTTP/1.1 connection handler for asyncio transport/protocol.
//!
//! Implements the asyncio Protocol interface using Rust-accelerated
//! parsing, scope building, and response writing.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Shared state for all protocol instances on this worker.
struct ProtocolShared {
    on_request: Py<PyAny>,
    interns: ScopeInterns,
    server_host: String,
    server_port: u16,
    active_requests: AtomicU32,
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

    /// Called by asyncio when data is received on the connection.
    fn data_received(&mut self, py: Python<'_>, data: &[u8]) -> PyResult<()> {
        let t0 = Instant::now();
        let requests = self
            .parser
            .feed(data)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        dispatch_metrics::record_parse(t0.elapsed().as_micros() as f64);

        for parsed in requests {
            self.dispatch_request(py, parsed)?;
        }
        Ok(())
    }

    /// Called by asyncio when the peer sends EOF.
    #[expect(clippy::unused_self, reason = "required by asyncio protocol interface")]
    fn eof_received(&self) -> bool {
        false
    }

    /// Called by asyncio when the connection is lost.
    fn connection_lost(&mut self, _py: Python<'_>, _exc: Option<&Bound<'_, PyAny>>) {
        self.transport = None;
        self.parser.reset();
        dispatch_metrics::dec_connections();
    }
}

impl RustProtocol {
    fn dispatch_request(&self, py: Python<'_>, parsed: ParsedRequest) -> PyResult<()> {
        let t_dispatch = Instant::now();
        let Some(transport) = &self.transport else {
            return Ok(());
        };

        let active = self.shared.active_requests.fetch_add(1, Ordering::Relaxed);
        if active >= MAX_CONCURRENT {
            self.shared.active_requests.fetch_sub(1, Ordering::Relaxed);
            write_503(py, transport)?;
            return Ok(());
        }
        dispatch_metrics::inc_active_requests();
        crate::telemetry::http::inc_active_requests();

        transport.call_method0(py, pyo3::intern!(py, "pause_reading"))?;

        let request_id = resolve_request_id(&parsed.head.headers);

        let t_scope = Instant::now();
        let scope = build_scope_from_parsed(
            py,
            &parsed,
            &self.shared.interns,
            &self.shared.server_host,
            self.shared.server_port,
            self.client_addr,
            &request_id,
        )?;
        dispatch_metrics::record_scope_build(t_scope.elapsed().as_micros() as f64);

        let t_receive = Instant::now();
        let receive = HttpReceive::new(py, parsed.body)?;
        dispatch_metrics::record_receive_build(t_receive.elapsed().as_micros() as f64);

        let method = parsed.head.method.as_str().to_owned();
        let path = parsed.head.path;

        let (request_span, trace_ctx) =
            crate::telemetry::http::begin_request_span(&request_id, &method, &path);
        crate::telemetry::context::set_python_context(py, &trace_ctx)?;

        let transport_clone = transport.clone_ref(py);
        let on_complete = OnRequestComplete::create(
            py,
            transport_clone,
            Arc::clone(&self.shared),
            t_dispatch,
            method,
            path,
            request_span,
        )?;
        let send =
            RustResponseWriter::new(py, transport.clone_ref(py), Some(on_complete.into_any()))?;

        self.shared.on_request.call1(py, (scope, receive, send))?;
        dispatch_metrics::record_dispatch_total(t_dispatch.elapsed().as_micros() as f64);
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
}

crate::opaque_debug!(OnRequestComplete);

impl OnRequestComplete {
    fn create(
        py: Python<'_>,
        transport: Py<PyAny>,
        shared: Arc<ProtocolShared>,
        dispatch_start: Instant,
        method: String,
        path: String,
        request_span: tracing::Span,
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

        self.transport
            .call_method0(py, pyo3::intern!(py, "resume_reading"))?;
        self.shared.active_requests.fetch_sub(1, Ordering::Relaxed);
        dispatch_metrics::dec_active_requests();
        crate::telemetry::http::dec_active_requests();
        Ok(())
    }
}

// ── HttpReceive ──────────────────────────────────────────────────────

/// ASGI `receive` callable for non-streaming HTTP requests.
///
/// First call returns the request body immediately via
/// `ResolvedAwaitableWithValue`. Subsequent calls return a pending
/// future (disconnect sentinel).
#[pyclass(module = "apx._core", freelist = 64)]
pub struct HttpReceive {
    body: std::sync::Mutex<Option<Bytes>>,
}

crate::opaque_debug!(HttpReceive);

impl HttpReceive {
    fn new(py: Python<'_>, body: Bytes) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                body: std::sync::Mutex::new(Some(body)),
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
fn build_scope_from_parsed(
    py: Python<'_>,
    parsed: &ParsedRequest,
    interns: &ScopeInterns,
    server_host: &str,
    server_port: u16,
    client_addr: Option<SocketAddr>,
    request_id: &str,
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

    set_headers_from_parsed(py, &scope, &parsed.head, interns, request_id)?;
    set_addresses(py, &scope, interns, server_host, server_port, client_addr)?;
    scope.set_item(
        interns.keys.path_params.bind(py),
        interns.empty_dict.bind(py),
    )?;
    scope.set_item(interns.keys.state.bind(py), PyDict::new(py))?;

    Ok(scope.unbind())
}

/// Extract existing `x-request-id` from headers or generate a UUID v4.
fn resolve_request_id(headers: &[(Bytes, Bytes)]) -> String {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(b"x-request-id")
            && let Ok(s) = std::str::from_utf8(value)
        {
            return s.to_owned();
        }
    }
    generate_uuid_v4()
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
) -> PyResult<()> {
    let has_request_id = head
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(b"x-request-id"));

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
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

/// Try to use the header intern cache, falling back to `PyBytes::new`.
fn intern_header_name<'py>(
    py: Python<'py>,
    name: &Bytes,
    interns: &ScopeInterns,
) -> Bound<'py, PyBytes> {
    let name_lower = name.to_ascii_lowercase();
    for (cached_name, cached_py) in &interns.headers.map {
        if cached_name.as_str().as_bytes() == name_lower.as_slice() {
            return cached_py.bind(py).clone();
        }
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
    if !input.contains('%') {
        return Cow::Borrowed(input);
    }
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.as_bytes().iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                if let (Some(hv), Some(lv)) = (hex_val(h), hex_val(l)) {
                    bytes.push(hv << 4 | lv);
                    continue;
                }
                bytes.extend_from_slice(&[b'%', h, l]);
            } else {
                bytes.push(b'%');
                if let Some(h) = hi {
                    bytes.push(h);
                }
            }
        } else {
            bytes.push(b);
        }
    }
    match String::from_utf8(bytes) {
        Ok(s) => Cow::Owned(s),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Convert a hex ASCII char to its value.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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
    format!("{host}:{port}").parse().ok()
}

/// Write a 503 Service Unavailable response directly.
fn write_503(py: Python<'_>, transport: &Py<PyAny>) -> PyResult<()> {
    let body = b"Service Unavailable";
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: {}\r\ncontent-type: text/plain\r\n\r\nService Unavailable",
        body.len(),
    );
    let py_bytes = PyBytes::new(py, response.as_bytes());
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
