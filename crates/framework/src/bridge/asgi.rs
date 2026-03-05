//! ASGI protocol primitives backed by Rust.
//!
//! Provides `AsgiReceive`, `AsgiSend` (Python callables), and `build_http_scope`
//! for constructing ASGI HTTP scope dicts from [`InboundRequest`].
//!
//! These types enable Starlette's `Request`, `StreamingResponse`, and `WebSocket`
//! to work unmodified against a Rust-backed ASGI server.

use crate::transport::types::InboundRequest;
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyDictMethods, PyList, PyTuple};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// ASGI protocol version string.
const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
const ASGI_SPEC_VERSION: &str = "2.3";

/// Default HTTP scheme (TLS detection is a future extension).
const DEFAULT_SCHEME: &str = "http";

// ── AsgiEvent ────────────────────────────────────────────────────────────

/// Parsed ASGI send event (Rust-side representation).
///
/// Pushed through a channel from [`AsgiSend`] (Python side) to the response
/// collector (Rust side) that assembles the final HTTP response or relays
/// WebSocket frames.
#[derive(Debug)]
pub enum AsgiEvent {
    /// `http.response.start` — status code and headers.
    ResponseStart {
        /// HTTP status code.
        status: u16,
        /// Response headers as raw byte pairs.
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// `http.response.body` — body chunk with continuation flag.
    ResponseBody {
        /// Body bytes.
        body: Bytes,
        /// Whether more body chunks follow.
        more_body: bool,
    },
    /// `websocket.accept` — server accepts the WebSocket connection.
    WsAccept {
        /// Optional subprotocol.
        subprotocol: Option<String>,
        /// Response headers as raw byte pairs.
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// `websocket.send` — server sends a frame to the client.
    WsSend {
        /// Text frame payload.
        text: Option<String>,
        /// Binary frame payload.
        bytes: Option<Vec<u8>>,
    },
    /// `websocket.close` — server closes the connection.
    WsClose {
        /// WebSocket close code (default 1000).
        code: u16,
    },
}

// ── AsgiReceive ──────────────────────────────────────────────────────────

/// ASGI `receive` callable backed by Rust.
///
/// For HTTP: first call returns `http.request` with the pre-buffered body,
/// subsequent calls return `http.disconnect`. Uses `Arc<Mutex<Option<Bytes>>>`
/// for interior mutability across await points.
#[pyclass(module = "apx._core")]
pub struct AsgiReceive {
    body: Arc<Mutex<Option<Bytes>>>,
}

impl std::fmt::Debug for AsgiReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiReceive").finish_non_exhaustive()
    }
}

impl AsgiReceive {
    /// Create for an HTTP request with a known body.
    pub fn http(body: Bytes) -> Self {
        Self {
            body: Arc::new(Mutex::new(Some(body))),
        }
    }

    /// Create for an HTTP request with no body (GET, HEAD, DELETE).
    pub fn empty() -> Self {
        Self {
            body: Arc::new(Mutex::new(Some(Bytes::new()))),
        }
    }
}

#[pymethods]
impl AsgiReceive {
    /// Python: `event = await receive()`
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let body = Arc::clone(&self.body);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = body.lock().await;
            Python::attach(|py| build_receive_event(py, guard.take()))
        })
    }
}

/// Build the ASGI receive event dict.
fn build_receive_event(py: Python<'_>, body: Option<Bytes>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    match body {
        Some(bytes) => {
            dict.set_item("type", "http.request")?;
            dict.set_item("body", PyBytes::new(py, &bytes))?;
            dict.set_item("more_body", false)?;
        }
        None => {
            dict.set_item("type", "http.disconnect")?;
        }
    }
    Ok(dict.into_any().unbind())
}

// ── AsgiSend ─────────────────────────────────────────────────────────────

/// ASGI `send` callable backed by Rust.
///
/// Parses ASGI event dicts and pushes [`AsgiEvent`] through a tokio channel.
/// `mpsc::Sender` is `Clone + Send + Sync` so no `Arc<Mutex>` wrapping needed.
#[pyclass(module = "apx._core")]
pub struct AsgiSend {
    tx: mpsc::Sender<AsgiEvent>,
}

impl std::fmt::Debug for AsgiSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiSend").finish_non_exhaustive()
    }
}

impl AsgiSend {
    /// Create a new `AsgiSend` backed by the given channel sender.
    pub fn new(tx: mpsc::Sender<AsgiEvent>) -> Self {
        Self { tx }
    }
}

#[pymethods]
impl AsgiSend {
    /// Python: `await send({"type": "http.response.start", ...})`
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let parsed = parse_asgi_send_event(&event)?;
        let tx = self.tx.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tx.send(parsed).await.map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err("response channel closed")
            })?;
            Python::attach(|py| Ok(py.None()))
        })
    }
}

// ── WebSocket incoming events ────────────────────────────────────────────

/// Incoming WebSocket event from the client (axum WS → Python handler).
#[derive(Debug)]
pub enum WsIncomingEvent {
    /// `websocket.connect` — initial connection event.
    Connect,
    /// `websocket.receive` — client sent a text or binary frame.
    Receive {
        /// Text frame payload.
        text: Option<String>,
        /// Binary frame payload.
        bytes: Option<Vec<u8>>,
    },
    /// `websocket.disconnect` — client disconnected.
    Disconnect {
        /// WebSocket close code (default 1000).
        code: u16,
    },
}

/// ASGI `receive` callable for WebSocket connections.
///
/// Returns ASGI dicts for `websocket.connect`, `websocket.receive`,
/// and `websocket.disconnect` events by reading from a channel fed
/// by the axum WebSocket frame forwarder.
#[pyclass(module = "apx._core")]
pub struct AsgiWsReceive {
    rx: Arc<Mutex<mpsc::Receiver<WsIncomingEvent>>>,
}

impl std::fmt::Debug for AsgiWsReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiWsReceive").finish_non_exhaustive()
    }
}

impl AsgiWsReceive {
    /// Create a new WebSocket receive callable.
    pub fn new(rx: mpsc::Receiver<WsIncomingEvent>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }
}

#[pymethods]
impl AsgiWsReceive {
    /// Python: `event = await receive()`
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = rx.lock().await;
            let event = guard.recv().await;
            Python::attach(|py| build_ws_receive_event(py, event))
        })
    }
}

/// Build an ASGI WebSocket receive event dict.
fn build_ws_receive_event(py: Python<'_>, event: Option<WsIncomingEvent>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    match event {
        Some(WsIncomingEvent::Connect) => {
            dict.set_item("type", "websocket.connect")?;
        }
        Some(WsIncomingEvent::Receive { text, bytes }) => {
            dict.set_item("type", "websocket.receive")?;
            if let Some(t) = text {
                dict.set_item("text", t)?;
            }
            if let Some(b) = bytes {
                dict.set_item("bytes", PyBytes::new(py, &b))?;
            }
        }
        Some(WsIncomingEvent::Disconnect { code }) => {
            dict.set_item("type", "websocket.disconnect")?;
            dict.set_item("code", code)?;
        }
        None => {
            dict.set_item("type", "websocket.disconnect")?;
            dict.set_item("code", 1000u16)?;
        }
    }
    Ok(dict.into_any().unbind())
}

// ── Parse helpers ────────────────────────────────────────────────────────

/// Parse an ASGI send event dict into a typed [`AsgiEvent`].
fn parse_asgi_send_event(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let event_type: String = event
        .get_item("type")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?
        .extract()?;

    match event_type.as_str() {
        "http.response.start" => parse_response_start(event),
        "http.response.body" => parse_response_body(event),
        "websocket.accept" => parse_ws_accept(event),
        "websocket.send" => parse_ws_send(event),
        "websocket.close" => parse_ws_close(event),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unsupported ASGI event type: {other}"
        ))),
    }
}

/// Parse `http.response.start` — extract status and headers.
fn parse_response_start(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let status: u16 = event
        .get_item("status")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("status"))?
        .extract()?;
    let headers = extract_header_list(event)?;
    Ok(AsgiEvent::ResponseStart { status, headers })
}

/// Parse `http.response.body` — extract body bytes and more_body flag.
fn parse_response_body(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let body: Vec<u8> = event
        .get_item("body")?
        .map(|b| b.extract())
        .transpose()?
        .unwrap_or_default();
    let more_body: bool = event
        .get_item("more_body")?
        .map(|b| b.extract())
        .transpose()?
        .unwrap_or(false);
    Ok(AsgiEvent::ResponseBody {
        body: Bytes::from(body),
        more_body,
    })
}

/// Parse `websocket.accept` — extract optional subprotocol and headers.
fn parse_ws_accept(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let subprotocol: Option<String> = event
        .get_item("subprotocol")?
        .and_then(|v| v.extract().ok());
    let headers = extract_header_list(event)?;
    Ok(AsgiEvent::WsAccept {
        subprotocol,
        headers,
    })
}

/// Parse `websocket.send` — extract text or binary payload.
fn parse_ws_send(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let text: Option<String> = event.get_item("text")?.and_then(|v| v.extract().ok());
    let bytes: Option<Vec<u8>> = event.get_item("bytes")?.and_then(|v| v.extract().ok());
    Ok(AsgiEvent::WsSend { text, bytes })
}

/// Parse `websocket.close` — extract close code.
fn parse_ws_close(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let code: u16 = event
        .get_item("code")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(1000);
    Ok(AsgiEvent::WsClose { code })
}

/// Parse ASGI headers list: `[(b"name", b"value"), ...]`.
fn extract_header_list(event: &Bound<'_, PyDict>) -> PyResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let Some(list) = event.get_item("headers")? else {
        return Ok(Vec::new());
    };
    list.try_iter()?
        .map(|item| {
            let tuple = item?;
            let name: Vec<u8> = tuple.get_item(0)?.extract()?;
            let value: Vec<u8> = tuple.get_item(1)?.extract()?;
            Ok((name, value))
        })
        .collect()
}

// ── build_http_scope ─────────────────────────────────────────────────────

/// Construct an ASGI HTTP scope dict from an [`InboundRequest`].
///
/// This is the bridge between the transport-neutral request abstraction
/// and the ASGI protocol. It must never receive hyper/axum types directly.
pub fn build_http_scope(py: Python<'_>, request: &InboundRequest) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    set_scope_metadata(py, &dict)?;
    set_scope_request_fields(py, &dict, request)?;
    set_scope_headers(py, &dict, request)?;
    set_scope_addresses(py, &dict, request)?;
    set_scope_path_params(py, &dict, request)?;
    dict.set_item("state", PyDict::new(py))?;
    Ok(dict.unbind())
}

/// Construct an ASGI WebSocket scope dict from an [`InboundRequest`].
///
/// Similar to [`build_http_scope`] but sets `type: "websocket"` and `scheme: "ws"`.
/// No body-related fields.
pub fn build_ws_scope(py: Python<'_>, request: &InboundRequest) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    set_ws_scope_metadata(py, &dict)?;
    set_ws_scope_request_fields(py, &dict, request)?;
    set_scope_headers(py, &dict, request)?;
    set_scope_addresses(py, &dict, request)?;
    set_scope_path_params(py, &dict, request)?;
    dict.set_item("state", PyDict::new(py))?;
    Ok(dict.unbind())
}

/// Default WebSocket scheme.
const WS_SCHEME: &str = "ws";

/// Set ASGI WebSocket scope metadata fields.
fn set_ws_scope_metadata(py: Python<'_>, dict: &Bound<'_, PyDict>) -> PyResult<()> {
    dict.set_item("type", "websocket")?;
    let asgi = PyDict::new(py);
    asgi.set_item("version", ASGI_VERSION)?;
    asgi.set_item("spec_version", ASGI_SPEC_VERSION)?;
    dict.set_item("asgi", asgi)?;
    dict.set_item("scheme", WS_SCHEME)?;
    dict.set_item("root_path", "")?;
    Ok(())
}

/// Set WebSocket request-specific scope fields.
fn set_ws_scope_request_fields(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
) -> PyResult<()> {
    dict.set_item("http_version", request.protocol.as_asgi_version())?;
    dict.set_item("path", &request.path)?;
    dict.set_item("raw_path", PyBytes::new(py, request.path.as_bytes()))?;
    dict.set_item("query_string", PyBytes::new(py, &request.query_string))?;
    Ok(())
}

/// Set ASGI scope metadata fields: type, asgi, http_version, scheme, root_path.
fn set_scope_metadata(py: Python<'_>, dict: &Bound<'_, PyDict>) -> PyResult<()> {
    dict.set_item("type", "http")?;
    let asgi = PyDict::new(py);
    asgi.set_item("version", ASGI_VERSION)?;
    asgi.set_item("spec_version", ASGI_SPEC_VERSION)?;
    dict.set_item("asgi", asgi)?;
    dict.set_item("scheme", DEFAULT_SCHEME)?;
    dict.set_item("root_path", "")?;
    Ok(())
}

/// Set request-specific scope fields: http_version, method, path, raw_path, query_string.
fn set_scope_request_fields(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
) -> PyResult<()> {
    dict.set_item("http_version", request.protocol.as_asgi_version())?;
    dict.set_item("method", request.method.as_str())?;
    dict.set_item("path", &request.path)?;
    dict.set_item("raw_path", PyBytes::new(py, request.path.as_bytes()))?;
    dict.set_item("query_string", PyBytes::new(py, &request.query_string))?;
    Ok(())
}

/// Set ASGI headers as a list of `(bytes, bytes)` tuples.
fn set_scope_headers(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
) -> PyResult<()> {
    let headers_list = PyList::empty(py);
    for (name, value) in &request.headers {
        let n = PyBytes::new(py, name.as_str().as_bytes());
        let v = PyBytes::new(py, value.as_bytes());
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        headers_list.append(pair)?;
    }
    dict.set_item("headers", headers_list)?;
    Ok(())
}

/// Set server and client address tuples in scope.
fn set_scope_addresses(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
) -> PyResult<()> {
    dict.set_item(
        "server",
        (
            request.server_addr.ip().to_string(),
            request.server_addr.port(),
        ),
    )?;
    match request.client_addr {
        Some(addr) => dict.set_item("client", (addr.ip().to_string(), addr.port()))?,
        None => dict.set_item("client", py.None())?,
    }
    Ok(())
}

/// Set path_params dict in scope (Starlette reads `scope["path_params"]`).
fn set_scope_path_params(
    _py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
) -> PyResult<()> {
    let pp = PyDict::new(dict.py());
    for (k, v) in &request.path_params {
        pp.set_item(k.as_str(), v.as_str())?;
    }
    dict.set_item("path_params", pp)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::transport::types::{BodyStream, ProtocolVersion, TransportKind};
    use http::header::HeaderMap;
    use std::net::SocketAddr;

    // ── Pure Rust tests ──────────────────────────────────────────────────

    #[test]
    fn asgi_event_debug_response_start() {
        let event = AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"text/plain".to_vec())],
        };
        let dbg = format!("{event:?}");
        assert!(dbg.contains("ResponseStart"));
        assert!(dbg.contains("200"));
    }

    #[test]
    fn asgi_event_debug_response_body() {
        let event = AsgiEvent::ResponseBody {
            body: Bytes::from("hello"),
            more_body: false,
        };
        let dbg = format!("{event:?}");
        assert!(dbg.contains("ResponseBody"));
    }

    #[test]
    fn asgi_receive_debug() {
        let recv = AsgiReceive::empty();
        let dbg = format!("{recv:?}");
        assert!(dbg.contains("AsgiReceive"));
    }

    #[test]
    fn asgi_send_debug() {
        let (tx, _rx) = mpsc::channel(1);
        let send = AsgiSend::new(tx);
        let dbg = format!("{send:?}");
        assert!(dbg.contains("AsgiSend"));
    }

    // ── Helper ───────────────────────────────────────────────────────────

    /// Initialize the Python interpreter (idempotent).
    fn init_python() {
        Python::initialize();
    }

    fn make_inbound_request(
        method: http::Method,
        path: &str,
        query: &[u8],
        headers: HeaderMap,
        path_params: Vec<(String, String)>,
        client_addr: Option<SocketAddr>,
    ) -> InboundRequest {
        InboundRequest::new(
            method,
            path.to_owned(),
            Bytes::copy_from_slice(query),
            headers,
            BodyStream::Empty,
            ProtocolVersion::Http11,
            TransportKind::Tcp,
            client_addr,
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            path_params,
            http::Extensions::new(),
        )
    }

    // ── build_http_scope tests (require Python) ──────────────────────────

    #[test]
    fn scope_basic_fields() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            assert_eq!(
                scope
                    .get_item("type")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "http"
            );
            assert_eq!(
                scope
                    .get_item("method")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "GET"
            );
            assert_eq!(
                scope
                    .get_item("path")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "/"
            );
            assert_eq!(
                scope
                    .get_item("scheme")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "http"
            );
            assert_eq!(
                scope
                    .get_item("root_path")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                ""
            );
            // asgi version
            let asgi = scope.get_item("asgi").unwrap().unwrap();
            assert_eq!(
                asgi.get_item("version")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "3.0"
            );
            assert_eq!(
                asgi.get_item("spec_version")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "2.3"
            );
        });
    }

    #[test]
    fn scope_protocol_versions() {
        init_python();
        for (version, expected) in [
            (ProtocolVersion::Http10, "1.0"),
            (ProtocolVersion::Http11, "1.1"),
            (ProtocolVersion::H2, "2"),
        ] {
            let req = InboundRequest::new(
                http::Method::GET,
                "/".to_owned(),
                Bytes::new(),
                HeaderMap::new(),
                BodyStream::Empty,
                version,
                TransportKind::Tcp,
                None,
                SocketAddr::from(([127, 0, 0, 1], 8080)),
                Vec::new(),
                http::Extensions::new(),
            );
            Python::attach(|py| {
                let scope = build_http_scope(py, &req).unwrap();
                let scope = scope.bind(py);
                let http_version: String = scope
                    .get_item("http_version")
                    .unwrap()
                    .unwrap()
                    .extract()
                    .unwrap();
                assert_eq!(http_version, expected, "version {version:?}");
            });
        }
    }

    #[test]
    fn scope_with_query_string() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/search",
            b"q=hello&page=1",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let qs: Vec<u8> = scope
                .get_item("query_string")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(qs, b"q=hello&page=1");
        });
    }

    #[test]
    fn scope_with_headers() {
        init_python();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-custom", "value".parse().unwrap());
        let req = make_inbound_request(http::Method::POST, "/api", b"", headers, Vec::new(), None);
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let headers_list = scope.get_item("headers").unwrap().unwrap();
            let len = headers_list.len().unwrap();
            assert_eq!(len, 2);
        });
    }

    #[test]
    fn scope_with_path_params() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/items/42",
            b"",
            HeaderMap::new(),
            vec![("item_id".to_owned(), "42".to_owned())],
            None,
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let pp = scope.get_item("path_params").unwrap().unwrap();
            let val: String = pp.get_item("item_id").unwrap().extract().unwrap();
            assert_eq!(val, "42");
        });
    }

    #[test]
    fn scope_with_client_addr() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            Some(SocketAddr::from(([192, 168, 1, 100], 12345))),
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let client = scope.get_item("client").unwrap().unwrap();
            let host: String = client.get_item(0).unwrap().extract().unwrap();
            let port: u16 = client.get_item(1).unwrap().extract().unwrap();
            assert_eq!(host, "192.168.1.100");
            assert_eq!(port, 12345);
        });
    }

    #[test]
    fn scope_no_client() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let client = scope.get_item("client").unwrap().unwrap();
            assert!(client.is_none());
        });
    }

    #[test]
    fn scope_server_addr() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        Python::attach(|py| {
            let scope = build_http_scope(py, &req).unwrap();
            let scope = scope.bind(py);
            let server = scope.get_item("server").unwrap().unwrap();
            let host: String = server.get_item(0).unwrap().extract().unwrap();
            let port: u16 = server.get_item(1).unwrap().extract().unwrap();
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 8080);
        });
    }

    // ── AsgiReceive logic tests ─────────────────────────────────────────

    #[tokio::test]
    async fn receive_http_body_then_disconnect() {
        init_python();
        let body = Arc::new(Mutex::new(Some(Bytes::from("hello"))));

        // First call: http.request with body
        let taken = body.lock().await.take();
        Python::attach(|py| {
            let result = build_receive_event(py, taken).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.request");
            let body_bytes: Vec<u8> = dict.get_item("body").unwrap().extract().unwrap();
            assert_eq!(body_bytes, b"hello");
            let more: bool = dict.get_item("more_body").unwrap().extract().unwrap();
            assert!(!more);
        });

        // Second call: http.disconnect
        let taken = body.lock().await.take();
        Python::attach(|py| {
            let result = build_receive_event(py, taken).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.disconnect");
        });
    }

    #[test]
    fn receive_empty_body() {
        init_python();
        Python::attach(|py| {
            let result = build_receive_event(py, Some(Bytes::new())).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.request");
            let body_bytes: Vec<u8> = dict.get_item("body").unwrap().extract().unwrap();
            assert!(body_bytes.is_empty());
        });
    }

    // ── AsgiSend parse + channel tests ───────────────────────────────────

    #[test]
    fn parse_response_start_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.start").unwrap();
            dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            let h = PyTuple::new(
                py,
                [
                    PyBytes::new(py, b"content-type").into_any(),
                    PyBytes::new(py, b"text/plain").into_any(),
                ],
            )
            .unwrap();
            headers.append(h).unwrap();
            dict.set_item("headers", headers).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseStart { status, headers } => {
                    assert_eq!(status, 200);
                    assert_eq!(headers.len(), 1);
                    assert_eq!(headers[0].0, b"content-type");
                    assert_eq!(headers[0].1, b"text/plain");
                }
                other => panic!("expected ResponseStart, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_response_body_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.body").unwrap();
            dict.set_item("body", PyBytes::new(py, b"hello")).unwrap();
            dict.set_item("more_body", false).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseBody { body, more_body } => {
                    assert_eq!(body.as_ref(), b"hello");
                    assert!(!more_body);
                }
                other => panic!("expected ResponseBody, got {other:?}"),
            }
        });
    }

    #[tokio::test]
    async fn send_event_through_channel() {
        let (tx, mut rx) = mpsc::channel(4);
        let event = AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"text/plain".to_vec())],
        };
        tx.send(event).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert!(matches!(
            received,
            AsgiEvent::ResponseStart { status: 200, .. }
        ));
    }

    #[tokio::test]
    async fn send_channel_closed_returns_error() {
        let (tx, rx) = mpsc::channel::<AsgiEvent>(1);
        drop(rx);
        let event = AsgiEvent::ResponseBody {
            body: Bytes::from("x"),
            more_body: false,
        };
        assert!(tx.send(event).await.is_err());
    }

    #[test]
    fn send_unknown_event_type() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.unknown").unwrap();
            let result = parse_asgi_send_event(&dict);
            assert!(result.is_err());
            let err_str = result.unwrap_err().to_string();
            assert!(err_str.contains("unsupported ASGI event type"));
        });
    }

    #[test]
    fn send_missing_type_key() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            let result = parse_asgi_send_event(&dict);
            assert!(result.is_err());
        });
    }

    // ── WebSocket event parse tests ─────────────────────────────────────

    #[test]
    fn parse_ws_accept_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.accept").unwrap();
            dict.set_item("subprotocol", "graphql-ws").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsAccept {
                    subprotocol,
                    headers,
                } => {
                    assert_eq!(subprotocol.as_deref(), Some("graphql-ws"));
                    assert!(headers.is_empty());
                }
                other => panic!("expected WsAccept, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_send_text_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.send").unwrap();
            dict.set_item("text", "hello").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsSend { text, bytes } => {
                    assert_eq!(text.as_deref(), Some("hello"));
                    assert!(bytes.is_none());
                }
                other => panic!("expected WsSend, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_send_binary_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.send").unwrap();
            dict.set_item("bytes", PyBytes::new(py, b"\x01\x02\x03"))
                .unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsSend { text, bytes } => {
                    assert!(text.is_none());
                    assert_eq!(bytes.as_deref(), Some(b"\x01\x02\x03".as_slice()));
                }
                other => panic!("expected WsSend, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_close_event() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.close").unwrap();
            dict.set_item("code", 1001u16).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsClose { code } => {
                    assert_eq!(code, 1001);
                }
                other => panic!("expected WsClose, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_close_default_code() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.close").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsClose { code } => {
                    assert_eq!(code, 1000);
                }
                other => panic!("expected WsClose, got {other:?}"),
            }
        });
    }

    #[test]
    fn ws_incoming_event_debug() {
        let connect = WsIncomingEvent::Connect;
        assert!(format!("{connect:?}").contains("Connect"));

        let recv = WsIncomingEvent::Receive {
            text: Some("hello".to_owned()),
            bytes: None,
        };
        assert!(format!("{recv:?}").contains("Receive"));

        let disc = WsIncomingEvent::Disconnect { code: 1000 };
        assert!(format!("{disc:?}").contains("Disconnect"));
    }

    #[test]
    fn asgi_ws_receive_debug() {
        let (_tx, rx) = mpsc::channel(1);
        let recv = AsgiWsReceive::new(rx);
        let dbg = format!("{recv:?}");
        assert!(dbg.contains("AsgiWsReceive"));
    }

    // ── build_ws_scope tests ────────────────────────────────────────────

    #[test]
    fn build_ws_scope_basic() {
        init_python();
        let req = make_inbound_request(
            http::Method::GET,
            "/ws",
            b"token=abc",
            HeaderMap::new(),
            vec![("room".to_owned(), "main".to_owned())],
            Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
        );
        Python::attach(|py| {
            let scope = build_ws_scope(py, &req).unwrap();
            let scope = scope.bind(py);

            let scope_type: String = scope.get_item("type").unwrap().unwrap().extract().unwrap();
            assert_eq!(scope_type, "websocket");

            let scheme: String = scope
                .get_item("scheme")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(scheme, "ws");

            let path: String = scope.get_item("path").unwrap().unwrap().extract().unwrap();
            assert_eq!(path, "/ws");

            let qs: Vec<u8> = scope
                .get_item("query_string")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(qs, b"token=abc");

            // path params
            let pp = scope.get_item("path_params").unwrap().unwrap();
            let room: String = pp.get_item("room").unwrap().extract().unwrap();
            assert_eq!(room, "main");

            // no 'method' key (WS scope doesn't have method)
            assert!(scope.get_item("method").unwrap().is_none());
        });
    }

    // ── build_ws_receive_event tests ─────────────────────────────────────

    #[test]
    fn build_ws_receive_event_connect() {
        init_python();
        Python::attach(|py| {
            let result = build_ws_receive_event(py, Some(WsIncomingEvent::Connect)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.connect");
        });
    }

    #[test]
    fn build_ws_receive_event_receive_text() {
        init_python();
        Python::attach(|py| {
            let event = WsIncomingEvent::Receive {
                text: Some("hello".to_owned()),
                bytes: None,
            };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.receive");
            let text: String = dict.get_item("text").unwrap().extract().unwrap();
            assert_eq!(text, "hello");
        });
    }

    #[test]
    fn build_ws_receive_event_receive_bytes() {
        init_python();
        Python::attach(|py| {
            let event = WsIncomingEvent::Receive {
                text: None,
                bytes: Some(vec![0x01, 0x02, 0x03]),
            };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.receive");
            let bytes: Vec<u8> = dict.get_item("bytes").unwrap().extract().unwrap();
            assert_eq!(bytes, vec![0x01, 0x02, 0x03]);
        });
    }

    #[test]
    fn build_ws_receive_event_disconnect_with_code() {
        init_python();
        Python::attach(|py| {
            let event = WsIncomingEvent::Disconnect { code: 1001 };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.disconnect");
            let code: u16 = dict.get_item("code").unwrap().extract().unwrap();
            assert_eq!(code, 1001);
        });
    }

    #[test]
    fn build_ws_receive_event_channel_closed() {
        init_python();
        Python::attach(|py| {
            let result = build_ws_receive_event(py, None).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.disconnect");
            let code: u16 = dict.get_item("code").unwrap().extract().unwrap();
            assert_eq!(code, 1000);
        });
    }

    // ── parse edge case tests ────────────────────────────────────────────

    #[test]
    fn parse_response_body_missing_body_key() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.body").unwrap();
            // No "body" key, no "more_body" key — defaults to empty body, more_body=false
            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseBody { body, more_body } => {
                    assert!(body.is_empty());
                    assert!(!more_body);
                }
                other => panic!("expected ResponseBody, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_accept_no_subprotocol() {
        init_python();
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.accept").unwrap();
            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsAccept {
                    subprotocol,
                    headers,
                } => {
                    assert!(subprotocol.is_none());
                    assert!(headers.is_empty());
                }
                other => panic!("expected WsAccept, got {other:?}"),
            }
        });
    }
}
