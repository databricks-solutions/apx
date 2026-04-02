//! Sans-I/O HTTP/1.1 request parser.
//!
//! Pure parsing — no I/O, no Python, no async. Takes bytes in,
//! returns parsed request data out. Testable with `#[test]`.

use bytes::{Bytes, BytesMut};

/// Maximum number of HTTP headers supported per request.
pub const MAX_HEADERS: usize = 96;

/// HTTP method (small enum for the common methods).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// GET
    Get,
    /// POST
    Post,
    /// PUT
    Put,
    /// DELETE
    Delete,
    /// PATCH
    Patch,
    /// HEAD
    Head,
    /// OPTIONS
    Options,
    /// Any other method stored as a string.
    Other(String),
}

impl Method {
    fn from_str(s: &str) -> Self {
        match s {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "PATCH" => Self::Patch,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            other => Self::Other(other.to_owned()),
        }
    }

    /// ASGI-compatible method string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Other(s) => s,
        }
    }
}

/// HTTP protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    /// HTTP/1.0
    Http10,
    /// HTTP/1.1
    Http11,
}

/// Parsed request head (status line + headers).
#[derive(Debug, Clone)]
pub struct ParsedHead {
    /// HTTP method.
    pub method: Method,
    /// Request path without query string.
    pub path: String,
    /// Raw query string (without leading `?`), empty if none.
    pub query_string: Bytes,
    /// Header pairs as raw bytes.
    pub headers: Vec<(Bytes, Bytes)>,
    /// HTTP version.
    pub version: HttpVersion,
    /// Content-Length value, if present.
    pub content_length: Option<usize>,
    /// Whether the request includes `Expect: 100-continue`.
    pub expect_continue: bool,
}

/// A fully parsed HTTP request (head + body).
#[derive(Debug, Clone)]
pub struct ParsedRequest {
    /// Request head.
    pub head: ParsedHead,
    /// Request body (may be empty).
    pub body: Bytes,
}

/// Parser state machine.
#[derive(Debug)]
enum ParseState {
    /// Waiting for a complete request line + headers.
    AwaitingHead,
    /// Head is parsed, accumulating body bytes.
    AwaitingBody {
        /// Parsed head.
        head: ParsedHead,
        /// Remaining body bytes to read.
        remaining: usize,
    },
}

/// Incremental HTTP/1.1 request parser.
///
/// Accumulates bytes via `feed()` and returns zero or more complete
/// requests (supports HTTP pipelining).
#[derive(Debug)]
pub struct RequestParser {
    buf: BytesMut,
    state: ParseState,
}

/// Parser error.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// httparse returned an error (malformed request).
    #[error("invalid HTTP request: {0}")]
    Invalid(String),
}

impl RequestParser {
    /// Create a new parser.
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8192),
            state: ParseState::AwaitingHead,
        }
    }

    /// Reset the parser state, discarding any partial data.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.state = ParseState::AwaitingHead;
    }

    /// Feed bytes into the parser.
    ///
    /// Returns zero or more complete requests. Partial data is buffered
    /// for the next `feed()` call. Supports HTTP pipelining: a single
    /// `data_received` call can contain multiple requests.
    ///
    /// # Errors
    ///
    /// Returns `ParseError` if the data contains malformed HTTP.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<ParsedRequest>, ParseError> {
        self.buf.extend_from_slice(data);
        let mut requests = Vec::new();

        loop {
            match &self.state {
                ParseState::AwaitingHead => {
                    let Some((head, consumed)) = self.try_parse_head()? else {
                        break;
                    };
                    self.advance_buffer(consumed);
                    let content_length = head.content_length.unwrap_or(0);
                    if content_length == 0 {
                        requests.push(ParsedRequest {
                            head,
                            body: Bytes::new(),
                        });
                    } else {
                        self.state = ParseState::AwaitingBody {
                            head,
                            remaining: content_length,
                        };
                    }
                }
                ParseState::AwaitingBody { remaining, .. } => {
                    let remaining = *remaining;
                    if self.buf.len() < remaining {
                        break;
                    }
                    let body = Bytes::copy_from_slice(&self.buf[..remaining]);
                    self.advance_buffer(remaining);

                    let state = std::mem::replace(&mut self.state, ParseState::AwaitingHead);
                    if let ParseState::AwaitingBody { head, .. } = state {
                        requests.push(ParsedRequest { head, body });
                    }
                }
            }
        }

        Ok(requests)
    }

    /// Try to parse a complete request head from the buffer.
    ///
    /// Returns `None` if more data is needed.
    fn try_parse_head(&self) -> Result<Option<(ParsedHead, usize)>, ParseError> {
        let mut headers_buf = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut req = httparse::Request::new(&mut headers_buf);

        match req.parse(&self.buf) {
            Ok(httparse::Status::Complete(consumed)) => {
                let head = build_head(&req)?;
                Ok(Some((head, consumed)))
            }
            Ok(httparse::Status::Partial) => Ok(None),
            Err(e) => Err(ParseError::Invalid(e.to_string())),
        }
    }

    fn advance_buffer(&mut self, n: usize) {
        let _ = self.buf.split_to(n);
    }
}

/// Build a `ParsedHead` from a completed httparse request.
fn build_head(req: &httparse::Request<'_, '_>) -> Result<ParsedHead, ParseError> {
    let method_str = req.method.unwrap_or("GET");
    let method = Method::from_str(method_str);

    let raw_path = req.path.unwrap_or("/");
    let (path, query_string) = split_path_query(raw_path);

    let version = match req.version {
        Some(0) => HttpVersion::Http10,
        _ => HttpVersion::Http11,
    };

    let mut content_length = None;
    let mut expect_continue = false;
    let mut headers = Vec::with_capacity(req.headers.len());

    for header in req.headers.iter() {
        let name = Bytes::copy_from_slice(header.name.as_bytes());
        let value = Bytes::copy_from_slice(header.value);
        if header.name.eq_ignore_ascii_case("content-length")
            && let Ok(s) = std::str::from_utf8(header.value)
        {
            content_length = s.trim().parse().ok();
        }
        if header.name.eq_ignore_ascii_case("expect")
            && header.value.eq_ignore_ascii_case(b"100-continue")
        {
            expect_continue = true;
        }
        headers.push((name, value));
    }

    Ok(ParsedHead {
        method,
        path: path.to_owned(),
        query_string,
        headers,
        version,
        content_length,
        expect_continue,
    })
}

/// Split a raw path into path and query string.
fn split_path_query(raw: &str) -> (&str, Bytes) {
    match raw.find('?') {
        Some(pos) => {
            let path = &raw[..pos];
            let qs = &raw[pos + 1..];
            (path, Bytes::copy_from_slice(qs.as_bytes()))
        }
        None => (raw, Bytes::new()),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
mod tests {
    use super::*;

    fn simple_get() -> Vec<u8> {
        b"GET /hello?name=world HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec()
    }

    fn post_with_body() -> Vec<u8> {
        b"POST /data HTTP/1.1\r\nHost: localhost\r\nContent-Length: 13\r\n\r\nHello, world!"
            .to_vec()
    }

    #[test]
    fn test_parse_simple_get() {
        let mut parser = RequestParser::new();
        let requests = parser.feed(&simple_get()).expect("parse failed");
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.head.method, Method::Get);
        assert_eq!(req.head.path, "/hello");
        assert_eq!(req.head.query_string, "name=world");
        assert_eq!(req.head.version, HttpVersion::Http11);
        assert!(req.body.is_empty());
        assert_eq!(req.head.headers.len(), 1);
    }

    #[test]
    fn test_parse_post_with_body() {
        let mut parser = RequestParser::new();
        let requests = parser.feed(&post_with_body()).expect("parse failed");
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.head.method, Method::Post);
        assert_eq!(req.head.path, "/data");
        assert_eq!(req.body, "Hello, world!");
        assert_eq!(req.head.content_length, Some(13));
    }

    #[test]
    fn test_partial_head() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"GET /hello HTTP/1.1\r\nHost: loc")
            .expect("parse failed");
        assert!(requests.is_empty());

        let requests = parser.feed(b"alhost\r\n\r\n").expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].head.path, "/hello");
    }

    #[test]
    fn test_partial_body() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"POST /data HTTP/1.1\r\nContent-Length: 12\r\n\r\nHello")
            .expect("parse failed");
        assert!(requests.is_empty());

        let requests = parser.feed(b", wor").expect("parse failed");
        assert!(requests.is_empty());

        let requests = parser.feed(b"ld").expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body, "Hello, world");
    }

    #[test]
    fn test_pipelining() {
        let mut parser = RequestParser::new();
        let mut data = Vec::new();
        data.extend_from_slice(b"GET /a HTTP/1.1\r\nHost: h\r\n\r\n");
        data.extend_from_slice(b"GET /b HTTP/1.1\r\nHost: h\r\n\r\n");

        let requests = parser.feed(&data).expect("parse failed");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].head.path, "/a");
        assert_eq!(requests[1].head.path, "/b");
    }

    #[test]
    fn test_no_query_string() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"GET /path HTTP/1.1\r\nHost: h\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].head.query_string.is_empty());
    }

    #[test]
    fn test_http10() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"GET / HTTP/1.0\r\nHost: h\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].head.version, HttpVersion::Http10);
    }

    #[test]
    fn test_multiple_headers() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"GET / HTTP/1.1\r\nHost: h\r\nAccept: text/html\r\nX-Custom: val\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].head.headers.len(), 3);
    }

    #[test]
    fn test_malformed_request() {
        let mut parser = RequestParser::new();
        let result = parser.feed(b"INVALID\r\n\r\n");
        assert!(result.is_err() || result.expect("unexpected ok").is_empty());
    }

    #[test]
    fn test_reset() {
        let mut parser = RequestParser::new();
        parser.feed(b"GET /partial HTTP/1.1\r\n").expect("ok");
        parser.reset();
        let requests = parser
            .feed(b"GET /fresh HTTP/1.1\r\nHost: h\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].head.path, "/fresh");
    }

    #[test]
    fn test_zero_content_length() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"POST /data HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.is_empty());
    }

    #[test]
    fn test_expect_100_continue_detected() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(
                b"POST /upload HTTP/1.1\r\n\
                  Host: h\r\n\
                  Expect: 100-continue\r\n\
                  Content-Length: 5\r\n\r\n\
                  hello",
            )
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].head.expect_continue);
    }

    #[test]
    fn test_expect_100_continue_absent() {
        let mut parser = RequestParser::new();
        let requests = parser
            .feed(b"GET / HTTP/1.1\r\nHost: h\r\n\r\n")
            .expect("parse failed");
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].head.expect_continue);
    }
}
