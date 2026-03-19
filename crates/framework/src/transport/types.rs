//! Transport-neutral request and response types.
//!
//! These types sit between the protocol layer and the application layer.
//! They are the architectural pivot that keeps ASGI and dispatch transport-agnostic.
//!
//! `InboundRequest` / `OutboundResponse` are the sole interface between the
//! transport-specific code (hyper, future quinn) and the transport-agnostic
//! application code (routing, dispatch, ASGI adapter).

use bytes::{Bytes, BytesMut};
use http::header::HeaderMap;
use http_body::Frame;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Which transport carried this request.
///
/// Closed enum — new transports are added as variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind {
    /// TCP socket.
    Tcp,
    /// Unix domain socket.
    Unix,
    /// In-memory channel (tests).
    InMemory,
    // Quic,  // future
}

/// HTTP protocol version.
///
/// Tracked per-request so ASGI scope can set `http_version` correctly
/// and future h3 responses can set appropriate headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    /// HTTP/1.0.
    Http10,
    /// HTTP/1.1.
    Http11,
    /// HTTP/2.
    H2,
    // H3,  // future — added when QUIC transport lands
}

impl ProtocolVersion {
    /// ASGI spec string for `scope["http_version"]`.
    pub fn as_asgi_version(&self) -> &'static str {
        match self {
            Self::Http10 => "1.0",
            Self::Http11 => "1.1",
            Self::H2 => "2",
        }
    }
}

/// Error reading or limiting a request/response body.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    /// Body exceeded the configured size limit.
    #[error("body exceeds size limit of {limit} bytes")]
    TooLarge {
        /// Configured limit.
        limit: usize,
    },
    /// IO error while reading body stream.
    #[error("body read error: {0}")]
    Io(#[from] std::io::Error),
}

/// Transport-neutral request body.
///
/// Abstracts over pre-buffered bodies (HTTP/1.1) and streamed bodies
/// (HTTP/2 DATA frames, future HTTP/3 streams).
///
/// Rule: Body is always a stream interface, never a concrete type.
pub enum BodyStream {
    /// No body (GET, HEAD, DELETE), or body already taken via `take_body()`.
    Empty,
    /// Fully buffered body (small POST/PUT, already read by transport).
    Buffered(Bytes),
    /// Streaming body (large uploads, chunked transfer, h2/h3 streams).
    Stream(Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>>),
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("BodyStream::Empty"),
            Self::Buffered(b) => write!(f, "BodyStream::Buffered({} bytes)", b.len()),
            Self::Stream(_) => f.write_str("BodyStream::Stream(...)"),
        }
    }
}

impl BodyStream {
    /// Read the full body into a single `Bytes`, respecting the size limit.
    ///
    /// For `Stream` variant, collects chunks up to the limit.
    ///
    /// # Errors
    ///
    /// Returns `BodyError::TooLarge` if the collected body exceeds `limit`.
    pub async fn collect(self, limit: usize) -> Result<Bytes, BodyError> {
        match self {
            Self::Empty => Ok(Bytes::new()),
            Self::Buffered(b) => {
                if b.len() > limit {
                    return Err(BodyError::TooLarge { limit });
                }
                Ok(b)
            }
            Self::Stream(mut stream) => collect_stream(&mut stream, limit).await,
        }
    }

    /// True if there is no body content.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// Default initial capacity for streaming body collection.
///
/// Sized to hold a typical small API request body without reallocation.
/// Capped independently of the body size limit to avoid over-allocating
/// for endpoints with large limits but small actual bodies.
const STREAM_COLLECT_INITIAL_CAPACITY: usize = 4096;

/// Read the next chunk from the stream.
async fn next_chunk(
    stream: &mut Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
) -> Result<Option<Bytes>, BodyError> {
    match std::future::poll_fn(|cx| Pin::as_mut(stream).poll_next(cx)).await {
        Some(Ok(bytes)) => Ok(Some(bytes)),
        Some(Err(e)) => Err(BodyError::Io(e)),
        None => Ok(None),
    }
}

/// Collect a stream of bytes into a single `Bytes`, enforcing a size limit.
///
/// Fast path: single-chunk bodies (common for small JSON payloads) are returned
/// directly without any copy or concatenation. Multi-chunk bodies use `BytesMut`
/// for contiguous concatenation, then `freeze()` for zero-copy conversion to `Bytes`.
async fn collect_stream(
    stream: &mut Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    limit: usize,
) -> Result<Bytes, BodyError> {
    // Read the first chunk.
    let first = match next_chunk(stream).await? {
        Some(bytes) => {
            if bytes.len() > limit {
                return Err(BodyError::TooLarge { limit });
            }
            bytes
        }
        None => return Ok(Bytes::new()),
    };

    // Fast path: if the stream is exhausted after one chunk, return it directly (zero-copy).
    let Some(second) = next_chunk(stream).await? else {
        return Ok(first);
    };

    // Multi-chunk: concatenate into BytesMut.
    let mut buf = BytesMut::with_capacity(limit.min(STREAM_COLLECT_INITIAL_CAPACITY));
    buf.extend_from_slice(&first);

    if buf.len() + second.len() > limit {
        return Err(BodyError::TooLarge { limit });
    }
    buf.extend_from_slice(&second);

    loop {
        match next_chunk(stream).await? {
            Some(bytes) => {
                if buf.len() + bytes.len() > limit {
                    return Err(BodyError::TooLarge { limit });
                }
                buf.extend_from_slice(&bytes);
            }
            None => return Ok(buf.freeze()),
        }
    }
}

/// Transport-neutral HTTP request.
///
/// Constructed by the transport/protocol layer (hyper, future quinn).
/// Consumed by the application layer (routing, dispatch, ASGI adapter).
/// This is the architectural boundary between transport-specific and
/// transport-agnostic code.
///
/// Body ownership: the body is taken once via `take_body()` (returns the
/// `BodyStream` and replaces it with `Empty`). After that, the request
/// can still be borrowed for scope construction, header access, etc.
pub struct InboundRequest {
    /// HTTP method.
    pub method: http::Method,
    /// Request path (without query string).
    pub path: String,
    /// Raw query string bytes.
    pub query_string: Bytes,
    /// HTTP headers.
    pub headers: HeaderMap,
    /// Request body (private — use `take_body()`).
    body: BodyStream,
    /// HTTP protocol version.
    pub protocol: ProtocolVersion,
    /// Which transport carried this request.
    pub transport: TransportKind,
    /// Client socket address (if available).
    pub client_addr: Option<SocketAddr>,
    /// Server socket address.
    pub server_addr: SocketAddr,
    /// Path parameters extracted by the router (populated after routing).
    pub path_params: Vec<(String, String)>,
    /// Opaque extensions for middleware (trace ids, etc.).
    pub extensions: http::Extensions,
}

impl std::fmt::Debug for InboundRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboundRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("protocol", &self.protocol)
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl InboundRequest {
    /// Construct a new `InboundRequest`. Called by `transport/convert.rs`.
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor mirrors HTTP request fields"
    )]
    pub fn new(
        method: http::Method,
        path: String,
        query_string: Bytes,
        headers: HeaderMap,
        body: BodyStream,
        protocol: ProtocolVersion,
        transport: TransportKind,
        client_addr: Option<SocketAddr>,
        server_addr: SocketAddr,
        path_params: Vec<(String, String)>,
        extensions: http::Extensions,
    ) -> Self {
        Self {
            method,
            path,
            query_string,
            headers,
            body,
            protocol,
            transport,
            client_addr,
            server_addr,
            path_params,
            extensions,
        }
    }

    /// Take the body out, replacing it with `BodyStream::Empty`.
    ///
    /// Call this once before reading the body. After this, the request
    /// can still be borrowed for `build_http_scope`, header access, etc.
    pub fn take_body(&mut self) -> BodyStream {
        std::mem::replace(&mut self.body, BodyStream::Empty)
    }

    /// Whether the request still has a body (not yet taken).
    pub fn has_body(&self) -> bool {
        !self.body.is_empty()
    }
}

/// Transport-neutral HTTP response.
///
/// Constructed by dispatch/ASGI adapter. Consumed by transport layer
/// to write the response back over the wire.
pub struct OutboundResponse {
    /// HTTP status code.
    pub status: http::StatusCode,
    /// Response headers.
    pub headers: HeaderMap,
    /// Response body.
    pub body: ResponseBody,
    /// Matched route template extracted from the ASGI scope (e.g. `/users/{user_id}`).
    pub server_route: Option<String>,
}

impl std::fmt::Debug for OutboundResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Response body — either fixed or streaming.
pub enum ResponseBody {
    /// Complete body, known length.
    Fixed(Bytes),
    /// Streaming body (SSE, chunked, large responses).
    Stream(Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>>),
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(b) => write!(f, "ResponseBody::Fixed({} bytes)", b.len()),
            Self::Stream(_) => f.write_str("ResponseBody::Stream(...)"),
        }
    }
}

impl http_body::Body for ResponseBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match this {
            Self::Fixed(bytes) => {
                if bytes.is_empty() {
                    return Poll::Ready(None);
                }
                let data = std::mem::replace(bytes, Bytes::new());
                Poll::Ready(Some(Ok(Frame::data(data))))
            }
            Self::Stream(stream) => match Pin::as_mut(stream).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
                Poll::Ready(Some(Err(e))) => {
                    let err: Box<dyn std::error::Error + Send + Sync> = Box::new(e);
                    Poll::Ready(Some(Err(err)))
                }
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Fixed(bytes) => bytes.is_empty(),
            Self::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Self::Fixed(bytes) => http_body::SizeHint::with_exact(bytes.len() as u64),
            Self::Stream(_) => http_body::SizeHint::default(),
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn transport_kind_serde_roundtrip() {
        for kind in [
            TransportKind::Tcp,
            TransportKind::Unix,
            TransportKind::InMemory,
        ] {
            let json = serde_json::to_string(&kind).ok();
            assert!(json.is_some(), "serialize {kind:?}");
            let back: TransportKind =
                serde_json::from_str(json.as_deref().unwrap_or("")).unwrap_or(TransportKind::Tcp);
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn protocol_version_asgi_string() {
        assert_eq!(ProtocolVersion::Http10.as_asgi_version(), "1.0");
        assert_eq!(ProtocolVersion::Http11.as_asgi_version(), "1.1");
        assert_eq!(ProtocolVersion::H2.as_asgi_version(), "2");
    }

    #[test]
    fn body_stream_empty_is_empty() {
        assert!(BodyStream::Empty.is_empty());
        assert!(!BodyStream::Buffered(Bytes::from_static(b"x")).is_empty());
    }

    #[tokio::test]
    async fn body_stream_buffered_collect() {
        let body = BodyStream::Buffered(Bytes::from_static(b"hello"));
        let result = body.collect(1024).await;
        assert!(result.is_ok());
        assert_eq!(result.ok().as_deref(), Some(b"hello".as_slice()));
    }

    #[tokio::test]
    async fn body_stream_collect_exceeds_limit() {
        let body = BodyStream::Buffered(Bytes::from_static(b"hello world"));
        let result = body.collect(5).await;
        assert!(matches!(result, Err(BodyError::TooLarge { limit: 5 })));
    }

    #[test]
    fn inbound_request_construction() {
        let req = InboundRequest::new(
            http::Method::GET,
            "/test".to_owned(),
            Bytes::new(),
            HeaderMap::new(),
            BodyStream::Empty,
            ProtocolVersion::Http11,
            TransportKind::Tcp,
            None,
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            Vec::new(),
            http::Extensions::new(),
        );
        assert_eq!(req.method, http::Method::GET);
        assert_eq!(req.path, "/test");
        assert!(!req.has_body());
    }

    #[test]
    fn inbound_request_take_body() {
        let mut req = InboundRequest::new(
            http::Method::POST,
            "/upload".to_owned(),
            Bytes::new(),
            HeaderMap::new(),
            BodyStream::Buffered(Bytes::from_static(b"data")),
            ProtocolVersion::Http11,
            TransportKind::Tcp,
            None,
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            Vec::new(),
            http::Extensions::new(),
        );
        assert!(req.has_body());
        let body = req.take_body();
        assert!(!req.has_body());
        assert!(matches!(body, BodyStream::Buffered(_)));
    }

    #[test]
    fn outbound_response_construction() {
        let resp = OutboundResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBody::Fixed(Bytes::from_static(b"ok")),
            server_route: None,
        };
        assert_eq!(resp.status, http::StatusCode::OK);
    }

    #[test]
    fn response_body_fixed_vs_stream() {
        let fixed = ResponseBody::Fixed(Bytes::from_static(b"hello"));
        assert!(matches!(fixed, ResponseBody::Fixed(_)));

        let empty_stream = tokio_stream::empty::<Result<Bytes, std::io::Error>>();
        let stream_body = ResponseBody::Stream(Box::pin(empty_stream));
        assert!(matches!(stream_body, ResponseBody::Stream(_)));
    }

    #[tokio::test]
    async fn body_stream_empty_collect() {
        let body = BodyStream::Empty;
        let result = body.collect(0).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn body_stream_stream_collect_success() {
        let chunks = vec![Ok(Bytes::from("hello")), Ok(Bytes::from(" world"))];
        let stream = tokio_stream::iter(chunks);
        let body = BodyStream::Stream(Box::pin(stream));
        let result = body.collect(1024).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"hello world");
    }

    #[tokio::test]
    async fn body_stream_stream_collect_io_error() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from("ok")),
            Err(std::io::Error::other("stream failed")),
        ];
        let stream = tokio_stream::iter(chunks);
        let body = BodyStream::Stream(Box::pin(stream));
        let result = body.collect(1024).await;
        assert!(matches!(result, Err(BodyError::Io(_))));
    }

    #[tokio::test]
    async fn body_stream_stream_collect_over_limit() {
        let chunks = vec![
            Ok(Bytes::from(vec![0u8; 600])),
            Ok(Bytes::from(vec![0u8; 600])),
        ];
        let stream = tokio_stream::iter(chunks);
        let body = BodyStream::Stream(Box::pin(stream));
        let result = body.collect(1000).await;
        assert!(matches!(result, Err(BodyError::TooLarge { limit: 1000 })));
    }

    #[test]
    fn body_error_display_too_large() {
        let err = BodyError::TooLarge { limit: 1024 };
        let msg = format!("{err}");
        assert!(msg.contains("1024"));
    }

    #[test]
    fn body_error_display_io() {
        let err = BodyError::Io(std::io::Error::other("read fail"));
        let msg = format!("{err}");
        assert!(msg.contains("read fail"));
    }

    #[test]
    fn body_stream_debug_all_variants() {
        let empty_dbg = format!("{:?}", BodyStream::Empty);
        assert!(empty_dbg.contains("Empty"));

        let buf_dbg = format!("{:?}", BodyStream::Buffered(Bytes::from("hi")));
        assert!(buf_dbg.contains("2 bytes"));

        let stream = tokio_stream::empty::<Result<Bytes, std::io::Error>>();
        let stream_dbg = format!("{:?}", BodyStream::Stream(Box::pin(stream)));
        assert!(stream_dbg.contains("Stream"));
    }

    #[test]
    fn inbound_request_debug() {
        let req = InboundRequest::new(
            http::Method::POST,
            "/api/test".to_owned(),
            Bytes::new(),
            HeaderMap::new(),
            BodyStream::Empty,
            ProtocolVersion::Http11,
            TransportKind::Tcp,
            None,
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            Vec::new(),
            http::Extensions::new(),
        );
        let dbg = format!("{req:?}");
        assert!(dbg.contains("InboundRequest"));
        assert!(dbg.contains("POST"));
    }

    #[test]
    fn outbound_response_debug() {
        let resp = OutboundResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBody::Fixed(Bytes::from("ok")),
            server_route: None,
        };
        let dbg = format!("{resp:?}");
        assert!(dbg.contains("OutboundResponse"));
        assert!(dbg.contains("200"));
    }

    #[test]
    fn response_body_debug() {
        let fixed = ResponseBody::Fixed(Bytes::from("hello"));
        let dbg = format!("{fixed:?}");
        assert!(dbg.contains("Fixed"));
        assert!(dbg.contains("5 bytes"));

        let stream = tokio_stream::empty::<Result<Bytes, std::io::Error>>();
        let stream_body = ResponseBody::Stream(Box::pin(stream));
        let dbg = format!("{stream_body:?}");
        assert!(dbg.contains("Stream"));
    }

    // ── http_body::Body impl tests ──────────────────────────────────────

    #[tokio::test]
    async fn response_body_fixed_yields_data_then_none() {
        use http_body::Body;
        let mut body = ResponseBody::Fixed(Bytes::from_static(b"hello"));
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"hello"));
        let end = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        assert!(end.is_none());
    }

    #[test]
    fn response_body_empty_fixed_is_end_stream() {
        use http_body::Body;
        let body = ResponseBody::Fixed(Bytes::new());
        assert!(body.is_end_stream());

        let non_empty = ResponseBody::Fixed(Bytes::from_static(b"x"));
        assert!(!non_empty.is_end_stream());
    }

    #[test]
    fn response_body_fixed_size_hint_exact() {
        use http_body::Body;
        let body = ResponseBody::Fixed(Bytes::from_static(b"hello"));
        let hint = body.size_hint();
        assert_eq!(hint.lower(), 5);
        assert_eq!(hint.upper(), Some(5));
    }

    #[tokio::test]
    async fn response_body_stream_yields_chunks() {
        use http_body::Body;
        let chunks = vec![Ok(Bytes::from("hel")), Ok(Bytes::from("lo"))];
        let stream = tokio_stream::iter(chunks);
        let mut body = ResponseBody::Stream(Box::pin(stream));

        let f1 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f1.into_data().unwrap(), Bytes::from("hel"));

        let f2 = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f2.into_data().unwrap(), Bytes::from("lo"));

        let end = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        assert!(end.is_none());
    }
}
