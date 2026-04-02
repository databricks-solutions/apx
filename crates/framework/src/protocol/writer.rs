//! HTTP/1.1 response writer backed by an asyncio transport.
//!
//! Builds HTTP response bytes and writes to the asyncio transport.
//! Sans-I/O core (`build_status_and_headers`, `parse_send_event`) is
//! testable with `#[test]`.

use std::cell::RefCell;
use std::time::Instant;

use bytes::{BufMut, Bytes, BytesMut};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use crate::asgi::scope::ResolvedAwaitable;
use crate::telemetry::dispatch_metrics;

// ── Date header cache ───────────────────────────────────────────────

thread_local! {
    static CACHED_DATE: RefCell<(Instant, Bytes)> = RefCell::new((
        Instant::now(),
        Bytes::from_static(b""),
    ));
}

/// RFC 7231 `Date` header, cached and refreshed every second.
fn cached_date_header() -> Bytes {
    CACHED_DATE.with(|cell| {
        let mut cached = cell.borrow_mut();
        if cached.0.elapsed().as_secs() >= 1 || cached.1.is_empty() {
            let now = httpdate::fmt_http_date(std::time::SystemTime::now());
            cached.1 = Bytes::from(format!("date: {now}\r\n"));
            cached.0 = Instant::now();
        }
        cached.1.clone()
    })
}

/// ASGI send event parsed from a Python dict.
#[derive(Debug)]
pub enum SendEvent {
    /// `http.response.start` — status code + headers.
    Start {
        /// HTTP status code.
        status: u16,
        /// Response headers as raw byte pairs.
        headers: Vec<(Bytes, Bytes)>,
    },
    /// `http.response.body` — body chunk.
    Body {
        /// Body bytes.
        data: Py<PyBytes>,
        /// Whether more body chunks will follow.
        more_body: bool,
    },
}

/// Writer state machine.
#[derive(Debug)]
enum WriteState {
    /// Waiting for `http.response.start`.
    AwaitingStart,
    /// Got start, waiting for first body chunk.
    HeadersPending {
        /// HTTP status code.
        status: u16,
        /// Response headers.
        headers: Vec<(Bytes, Bytes)>,
    },
    /// Streaming body chunks (with or without chunked encoding).
    Streaming { chunked: bool },
    /// Response complete.
    Done,
}

/// HTTP/1.1 response writer backed by an asyncio transport.
///
/// Implements the ASGI `send` callable. Builds HTTP response bytes
/// in Rust and writes them to `transport.write()`.
#[pyclass(module = "apx._core")]
pub struct RustResponseWriter {
    transport: Py<PyAny>,
    state: WriteState,
    resolved: Py<ResolvedAwaitable>,
    on_complete: Option<Py<PyAny>>,
    /// HTTP status code from `http.response.start`, for metrics.
    response_status: u16,
}

crate::opaque_debug!(RustResponseWriter);

impl RustResponseWriter {
    /// Create a new response writer.
    pub fn new(
        py: Python<'_>,
        transport: Py<PyAny>,
        on_complete: Option<Py<PyAny>>,
    ) -> PyResult<Py<Self>> {
        let resolved = Py::new(py, ResolvedAwaitable)?;
        Py::new(
            py,
            Self {
                transport,
                state: WriteState::AwaitingStart,
                resolved,
                on_complete,
                response_status: 0,
            },
        )
    }
}

#[pymethods]
impl RustResponseWriter {
    /// ASGI send callable.
    fn __call__(&mut self, py: Python<'_>, event: &Bound<'_, PyDict>) -> PyResult<Py<PyAny>> {
        let t0 = Instant::now();
        let parsed = parse_send_event(py, event)?;
        dispatch_metrics::record_send_parse(t0.elapsed().as_micros() as f64);

        match parsed {
            SendEvent::Start { status, headers } => {
                self.response_status = status;
                self.state = WriteState::HeadersPending { status, headers };
            }
            SendEvent::Body { data, more_body } => {
                self.write_body(py, &data, more_body)?;
            }
        }
        Ok(self.resolved.clone_ref(py).into_any())
    }

    /// Write a 500 error response directly (bypasses ASGI state machine).
    fn send_error(&mut self, py: Python<'_>, traceback: &str) -> PyResult<()> {
        let body = traceback.as_bytes();
        let headers = vec![(
            Bytes::from_static(b"content-type"),
            Bytes::from_static(b"text/plain; charset=utf-8"),
        )];
        let response = build_full_response(500, &headers, body);
        let py_bytes = PyBytes::new(py, &response);
        self.transport
            .call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))?;
        self.state = WriteState::Done;
        self.response_status = 500;
        self.signal_complete(py)?;
        Ok(())
    }
}

impl RustResponseWriter {
    fn write_body(&mut self, py: Python<'_>, data: &Py<PyBytes>, more_body: bool) -> PyResult<()> {
        match std::mem::replace(&mut self.state, WriteState::Done) {
            WriteState::HeadersPending { status, headers } => {
                self.write_first_body(py, status, &headers, data, more_body)?;
            }
            WriteState::Streaming { chunked } => {
                self.write_continuation(py, data, more_body, chunked)?;
            }
            _ => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "send: body before start or after done",
                ));
            }
        }
        Ok(())
    }

    fn write_first_body(
        &mut self,
        py: Python<'_>,
        status: u16,
        headers: &[(Bytes, Bytes)],
        data: &Py<PyBytes>,
        more_body: bool,
    ) -> PyResult<()> {
        let body_bytes = data.bind(py).as_bytes();
        let has_content_length = headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(b"content-length"));

        let chunked = more_body && !has_content_length;

        let t_build = Instant::now();
        let hdr_bytes = if chunked {
            build_status_and_headers_chunked(status, headers)
        } else if !more_body && !has_content_length {
            build_status_and_headers_with_length(status, headers, body_bytes.len())
        } else {
            build_status_and_headers(status, headers)
        };
        dispatch_metrics::record_response_build(t_build.elapsed().as_micros() as f64);

        const MERGE_THRESHOLD: usize = 65_536;

        let t_write = Instant::now();
        let write_result = if chunked {
            let mut buf = BytesMut::with_capacity(hdr_bytes.len() + body_bytes.len() + 32);
            buf.put_slice(&hdr_bytes);
            write_chunk(&mut buf, body_bytes);
            let py_bytes = PyBytes::new(py, &buf);
            self.transport
                .call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))
                .map(|_| ())
        } else if body_bytes.len() <= MERGE_THRESHOLD {
            let mut buf = BytesMut::with_capacity(hdr_bytes.len() + body_bytes.len());
            buf.put_slice(&hdr_bytes);
            buf.put_slice(body_bytes);
            let py_bytes = PyBytes::new(py, &buf);
            self.transport
                .call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))
                .map(|_| ())
        } else {
            let hdr_py = PyBytes::new(py, &hdr_bytes);
            self.transport
                .call_method1(py, pyo3::intern!(py, "write"), (hdr_py,))
                .and_then(|_| {
                    self.transport
                        .call_method1(py, pyo3::intern!(py, "write"), (data.bind(py),))
                        .map(|_| ())
                })
        };
        dispatch_metrics::record_response_write(t_write.elapsed().as_micros() as f64);

        if more_body {
            self.state = WriteState::Streaming { chunked };
        } else {
            // Always signal completion — even if write failed. The callback
            // decrements active_requests; skipping it leaks concurrency slots.
            self.signal_complete(py)?;
        }
        // Propagate write error after completion callback has fired.
        write_result
    }

    fn write_continuation(
        &mut self,
        py: Python<'_>,
        data: &Py<PyBytes>,
        more_body: bool,
        chunked: bool,
    ) -> PyResult<()> {
        let t_write = Instant::now();
        let body_bytes = data.bind(py).as_bytes();

        let write_result = if chunked {
            let terminator_len = if more_body { 0 } else { LAST_CHUNK.len() };
            let mut buf = BytesMut::with_capacity(body_bytes.len() + 32 + terminator_len);
            write_chunk(&mut buf, body_bytes);
            if !more_body {
                buf.put_slice(LAST_CHUNK);
            }
            let py_bytes = PyBytes::new(py, &buf);
            self.transport
                .call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))
                .map(|_| ())
        } else {
            self.transport
                .call_method1(py, pyo3::intern!(py, "write"), (data.bind(py),))
                .map(|_| ())
        };
        dispatch_metrics::record_response_write(t_write.elapsed().as_micros() as f64);

        if more_body {
            self.state = WriteState::Streaming { chunked };
        } else {
            // Always signal completion — even if write failed.
            self.signal_complete(py)?;
        }
        write_result
    }

    /// Signal response completion to the protocol layer.
    ///
    /// Must be called even when `transport.write()` fails — the callback
    /// decrements the active-request counter and resumes reading. Failing
    /// to call it leaks concurrency slots until `MAX_CONCURRENT` is hit
    /// and all new requests receive 503.
    fn signal_complete(&self, py: Python<'_>) -> PyResult<()> {
        if let Some(cb) = &self.on_complete {
            cb.call1(py, (self.response_status,))?;
        }
        Ok(())
    }
}

/// Parse an ASGI send event dict into a [`SendEvent`].
pub fn parse_send_event(py: Python<'_>, event: &Bound<'_, PyDict>) -> PyResult<SendEvent> {
    let type_obj = event
        .get_item(pyo3::intern!(py, "type"))?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'type' key"))?;
    let type_val: String = type_obj.extract()?;

    match type_val.as_str() {
        "http.response.start" => {
            let status: u16 = event
                .get_item(pyo3::intern!(py, "status"))?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'status' key"))?
                .extract()?;
            let headers = extract_response_headers(py, event)?;
            Ok(SendEvent::Start { status, headers })
        }
        "http.response.body" => {
            let body_obj = event
                .get_item(pyo3::intern!(py, "body"))?
                .unwrap_or_else(|| PyBytes::new(py, b"").into_any());
            let data: Py<PyBytes> = body_obj.extract()?;
            let more_body: bool = event
                .get_item(pyo3::intern!(py, "more_body"))?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(false);
            Ok(SendEvent::Body { data, more_body })
        }
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown send event type: {other}"
        ))),
    }
}

/// Extract response headers from the ASGI send event.
fn extract_response_headers(
    py: Python<'_>,
    event: &Bound<'_, PyDict>,
) -> PyResult<Vec<(Bytes, Bytes)>> {
    let headers_obj = event.get_item(pyo3::intern!(py, "headers"))?;
    let Some(headers_list) = headers_obj else {
        return Ok(Vec::new());
    };
    let list = headers_list.cast_into::<PyList>()?;
    let mut result = Vec::with_capacity(list.len());
    for item in list.iter() {
        let tuple = item.cast_into::<pyo3::types::PyTuple>()?;
        let name_obj = tuple.get_item(0)?.cast_into::<PyBytes>()?;
        let value_obj = tuple.get_item(1)?.cast_into::<PyBytes>()?;
        result.push((
            Bytes::copy_from_slice(name_obj.as_bytes()),
            Bytes::copy_from_slice(value_obj.as_bytes()),
        ));
    }
    Ok(result)
}

/// HTTP/1.1 chunked transfer encoding terminator.
const LAST_CHUNK: &[u8] = b"0\r\n\r\n";

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Write a single HTTP chunk frame: `{hex_len}\r\n{data}\r\n`.
fn write_chunk(buf: &mut BytesMut, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    write_hex(buf, data.len());
    buf.put_slice(b"\r\n");
    buf.put_slice(data);
    buf.put_slice(b"\r\n");
}

/// Write a `usize` as lowercase hex directly into `buf` (no heap alloc).
fn write_hex(buf: &mut BytesMut, mut n: usize) {
    if n == 0 {
        buf.put_u8(b'0');
        return;
    }
    let mut stack = [0u8; 16];
    let mut pos = stack.len();
    while n > 0 {
        pos -= 1;
        stack[pos] = HEX_DIGITS[n & 0xf];
        n >>= 4;
    }
    buf.put_slice(&stack[pos..]);
}

// ── Response head encoding ──────────────────────────────────────────

/// Write the status line for a given code, using cached bytes for
/// common codes to avoid per-response `to_string()` + concatenation.
fn write_status_line(buf: &mut BytesMut, status: u16) {
    match status {
        200 => {
            buf.put_slice(b"HTTP/1.1 200 OK\r\n");
            return;
        }
        201 => {
            buf.put_slice(b"HTTP/1.1 201 Created\r\n");
            return;
        }
        204 => {
            buf.put_slice(b"HTTP/1.1 204 No Content\r\n");
            return;
        }
        301 => {
            buf.put_slice(b"HTTP/1.1 301 Moved Permanently\r\n");
            return;
        }
        302 => {
            buf.put_slice(b"HTTP/1.1 302 Found\r\n");
            return;
        }
        304 => {
            buf.put_slice(b"HTTP/1.1 304 Not Modified\r\n");
            return;
        }
        307 => {
            buf.put_slice(b"HTTP/1.1 307 Temporary Redirect\r\n");
            return;
        }
        308 => {
            buf.put_slice(b"HTTP/1.1 308 Permanent Redirect\r\n");
            return;
        }
        400 => {
            buf.put_slice(b"HTTP/1.1 400 Bad Request\r\n");
            return;
        }
        401 => {
            buf.put_slice(b"HTTP/1.1 401 Unauthorized\r\n");
            return;
        }
        403 => {
            buf.put_slice(b"HTTP/1.1 403 Forbidden\r\n");
            return;
        }
        404 => {
            buf.put_slice(b"HTTP/1.1 404 Not Found\r\n");
            return;
        }
        405 => {
            buf.put_slice(b"HTTP/1.1 405 Method Not Allowed\r\n");
            return;
        }
        409 => {
            buf.put_slice(b"HTTP/1.1 409 Conflict\r\n");
            return;
        }
        422 => {
            buf.put_slice(b"HTTP/1.1 422 Unprocessable Entity\r\n");
            return;
        }
        429 => {
            buf.put_slice(b"HTTP/1.1 429 Too Many Requests\r\n");
            return;
        }
        500 => {
            buf.put_slice(b"HTTP/1.1 500 Internal Server Error\r\n");
            return;
        }
        502 => {
            buf.put_slice(b"HTTP/1.1 502 Bad Gateway\r\n");
            return;
        }
        503 => {
            buf.put_slice(b"HTTP/1.1 503 Service Unavailable\r\n");
            return;
        }
        504 => {
            buf.put_slice(b"HTTP/1.1 504 Gateway Timeout\r\n");
            return;
        }
        _ => {}
    }
    buf.put_slice(b"HTTP/1.1 ");
    buf.put_slice(status.to_string().as_bytes());
    buf.put_slice(b" ");
    buf.put_slice(reason_phrase(status).as_bytes());
    buf.put_slice(b"\r\n");
}

/// Encode status line + app headers + extra trailer headers + Date
/// into a `BytesMut`. Optionally appends body.
fn encode_head(
    status: u16,
    headers: &[(Bytes, Bytes)],
    extra: &[(&[u8], &[u8])],
    body: Option<&[u8]>,
) -> Bytes {
    let body_len = body.map_or(0, <[u8]>::len);
    let mut buf = BytesMut::with_capacity(256 + body_len);
    write_status_line(&mut buf, status);
    for (name, value) in headers {
        buf.put_slice(name);
        buf.put_slice(b": ");
        buf.put_slice(value);
        buf.put_slice(b"\r\n");
    }
    for &(name, value) in extra {
        buf.put_slice(name);
        buf.put_slice(b": ");
        buf.put_slice(value);
        buf.put_slice(b"\r\n");
    }
    buf.put_slice(&cached_date_header());
    buf.put_slice(b"\r\n");
    if let Some(b) = body {
        buf.put_slice(b);
    }
    buf.freeze()
}

/// Build the HTTP/1.1 status line + headers as bytes.
pub fn build_status_and_headers(status: u16, headers: &[(Bytes, Bytes)]) -> Bytes {
    encode_head(status, headers, &[], None)
}

/// Build status line + headers with `Transfer-Encoding: chunked`.
fn build_status_and_headers_chunked(status: u16, headers: &[(Bytes, Bytes)]) -> Bytes {
    encode_head(status, headers, &[(b"transfer-encoding", b"chunked")], None)
}

/// Build status line + headers with an auto-added `Content-Length`.
fn build_status_and_headers_with_length(
    status: u16,
    headers: &[(Bytes, Bytes)],
    body_len: usize,
) -> Bytes {
    let len_str = body_len.to_string();
    encode_head(
        status,
        headers,
        &[(b"content-length", len_str.as_bytes())],
        None,
    )
}

/// Build a complete HTTP/1.1 response (status + headers + body).
fn build_full_response(status: u16, headers: &[(Bytes, Bytes)], body: &[u8]) -> Bytes {
    let has_content_length = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(b"content-length"));
    if has_content_length {
        return encode_head(status, headers, &[], Some(body));
    }
    let len_str = body.len().to_string();
    encode_head(
        status,
        headers,
        &[(b"content-length", len_str.as_bytes())],
        Some(body),
    )
}

/// Standard HTTP reason phrase for common status codes.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_build_status_and_headers_200() {
        let headers = vec![(
            Bytes::from_static(b"content-type"),
            Bytes::from_static(b"text/html"),
        )];
        let result = build_status_and_headers(200, &headers);
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("content-type: text/html\r\n"));
        assert!(s.contains("date: "));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_build_status_and_headers_404() {
        let result = build_status_and_headers(404, &[]);
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("date: "));
    }

    #[test]
    fn test_build_full_response() {
        let headers = vec![(
            Bytes::from_static(b"content-type"),
            Bytes::from_static(b"text/plain"),
        )];
        let result = build_full_response(200, &headers, b"hello");
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.contains("content-length: 5\r\n"));
        assert!(s.contains("date: "));
        assert!(s.ends_with("hello"));
    }

    #[test]
    fn test_build_full_response_with_content_length() {
        let headers = vec![(
            Bytes::from_static(b"Content-Length"),
            Bytes::from_static(b"5"),
        )];
        let result = build_full_response(200, &headers, b"hello");
        let s = std::str::from_utf8(&result).expect("valid utf8");
        let lower = s.to_ascii_lowercase();
        let count = lower.matches("content-length").count();
        assert_eq!(count, 1, "should not add duplicate content-length");
    }

    #[test]
    fn test_multiple_headers() {
        let headers = vec![
            (Bytes::from_static(b"x-a"), Bytes::from_static(b"1")),
            (Bytes::from_static(b"x-b"), Bytes::from_static(b"2")),
        ];
        let result = build_status_and_headers(200, &headers);
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.contains("x-a: 1\r\n"));
        assert!(s.contains("x-b: 2\r\n"));
    }

    #[test]
    fn test_reason_phrase_unknown() {
        assert_eq!(reason_phrase(999), "Unknown");
    }

    #[test]
    fn test_build_chunked_headers() {
        let headers = vec![(
            Bytes::from_static(b"content-type"),
            Bytes::from_static(b"text/plain"),
        )];
        let result = build_status_and_headers_chunked(200, &headers);
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.contains("transfer-encoding: chunked\r\n"));
        assert!(s.contains("content-type: text/plain\r\n"));
        assert!(s.contains("date: "));
    }

    #[test]
    fn test_build_headers_with_length() {
        let headers = vec![(
            Bytes::from_static(b"content-type"),
            Bytes::from_static(b"text/plain"),
        )];
        let result = build_status_and_headers_with_length(200, &headers, 42);
        let s = std::str::from_utf8(&result).expect("valid utf8");
        assert!(s.contains("content-length: 42\r\n"));
        assert!(s.contains("date: "));
    }

    #[test]
    fn test_write_chunk_framing() {
        let mut buf = BytesMut::new();
        write_chunk(&mut buf, b"hello");
        assert_eq!(&buf[..], b"5\r\nhello\r\n");
    }

    #[test]
    fn test_write_chunk_empty_is_noop() {
        let mut buf = BytesMut::new();
        write_chunk(&mut buf, b"");
        assert!(buf.is_empty());
    }

    #[test]
    fn test_last_chunk_constant() {
        assert_eq!(LAST_CHUNK, b"0\r\n\r\n");
    }

    #[test]
    fn test_date_header_cached_within_second() {
        let a = cached_date_header();
        let b = cached_date_header();
        assert_eq!(a, b, "two calls within 1s must return the same value");
        assert!(a.starts_with(b"date: "));
        assert!(a.ends_with(b"\r\n"));
    }

    #[test]
    fn test_write_hex_zero() {
        let mut buf = BytesMut::new();
        write_hex(&mut buf, 0);
        assert_eq!(&buf[..], b"0");
    }

    #[test]
    fn test_write_hex_small() {
        let mut buf = BytesMut::new();
        write_hex(&mut buf, 255);
        assert_eq!(&buf[..], b"ff");
    }

    #[test]
    fn test_write_hex_large() {
        let mut buf = BytesMut::new();
        write_hex(&mut buf, 0x1a2b);
        assert_eq!(&buf[..], b"1a2b");
    }

    #[test]
    fn test_write_status_line_cached() {
        let mut buf = BytesMut::new();
        write_status_line(&mut buf, 200);
        assert_eq!(&buf[..], b"HTTP/1.1 200 OK\r\n");
    }

    #[test]
    fn test_write_status_line_uncached() {
        let mut buf = BytesMut::new();
        write_status_line(&mut buf, 418);
        assert_eq!(&buf[..], b"HTTP/1.1 418 Unknown\r\n");
    }
}
