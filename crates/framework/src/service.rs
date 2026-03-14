//! Hyper `Service` implementation with health probes, concurrency limiting,
//! and request timeout.
//!
//! `ApxService` is the HTTP layer between hyper and the application dispatch.
//! It short-circuits health probes, enforces a per-worker concurrency limit
//! via `Arc<Semaphore>`, and wraps dispatch in `tokio::time::timeout`.

use crate::dispatch::Dispatch;
use crate::telemetry::http::{self, ActiveRequestGuard};
use crate::transport::tcp::TcpListener;
use crate::transport::types::{
    BodyStream, InboundRequest, OutboundResponse, ProtocolVersion, ResponseBody, TransportKind,
};
use crate::websocket;
use bytes::Bytes;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::Service;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;

// ── Constants ────────────────────────────────────────────────────────────

/// Default request timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default max concurrent requests per worker.
const DEFAULT_MAX_CONCURRENT: usize = 256;

/// Health probe response: alive.
const HEALTH_ALIVE: &[u8] = br#"{"status":"alive"}"#;

/// Health probe response: ready.
const HEALTH_READY: &[u8] = br#"{"status":"ready"}"#;

/// JSON content type for health responses.
const JSON_CONTENT_TYPE: &str = "application/json";

// ── Config ───────────────────────────────────────────────────────────────

/// Configuration for the HTTP service layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Per-request timeout.
    pub timeout: Duration,
    /// Maximum concurrent requests per worker.
    pub max_concurrent: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }
}

// ── Service ──────────────────────────────────────────────────────────────

/// Hyper service implementation.
///
/// Cloned per-connection so that `client_addr` can be set for each connection.
/// The `dispatch` and `semaphore` are shared via `Arc`.
#[derive(Clone)]
pub struct ApxService {
    dispatch: Arc<dyn Dispatch>,
    semaphore: Arc<tokio::sync::Semaphore>,
    timeout: Duration,
    server_addr: SocketAddr,
    client_addr: Option<SocketAddr>,
}

impl ApxService {
    /// Create a new `ApxService`.
    pub fn new(
        dispatch: Arc<dyn Dispatch>,
        server_addr: SocketAddr,
        config: &ServiceConfig,
    ) -> Self {
        Self {
            dispatch,
            semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent)),
            timeout: config.timeout,
            server_addr,
            client_addr: None,
        }
    }

    /// Set the client address for this per-connection clone.
    pub fn with_client_addr(mut self, addr: SocketAddr) -> Self {
        self.client_addr = Some(addr);
        self
    }
}

impl std::fmt::Debug for ApxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApxService")
            .field("dispatch", &self.dispatch)
            .field("timeout", &self.timeout)
            .field("server_addr", &self.server_addr)
            .field("client_addr", &self.client_addr)
            .finish_non_exhaustive()
    }
}

impl Service<Request<Incoming>> for ApxService {
    type Response = Response<ResponseBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let this = self.clone();
        Box::pin(async move { Ok(this.handle(req).await) })
    }
}

// ── Request pipeline ─────────────────────────────────────────────────────

impl ApxService {
    /// Main request handler — orchestrates probe check, semaphore, timeout, dispatch.
    async fn handle(self, req: Request<Incoming>) -> Response<ResponseBody> {
        let method = req.method().as_str().to_owned();
        let scheme = "http"; // TODO: detect TLS in future

        // Health probe short-circuit.
        if let Some(probe_resp) = probe_response(req.uri().path()) {
            return probe_resp;
        }

        let _active = ActiveRequestGuard::enter(&method, scheme);
        let start = std::time::Instant::now();

        // Concurrency limit.
        let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
            let resp = error_response(hyper::StatusCode::SERVICE_UNAVAILABLE, "service overloaded");
            let elapsed = start.elapsed().as_secs_f64();
            http::record_duration(elapsed, &method, scheme, 503, "", Some("503"));
            return resp;
        };

        // WebSocket upgrade — must happen before consuming the request body.
        if websocket::is_websocket_upgrade(&req) {
            let path = req.uri().path().to_owned();
            let response = self
                .dispatch
                .dispatch_ws(req, self.server_addr, self.client_addr)
                .await;
            let status = response.status().as_u16();
            let elapsed = start.elapsed().as_secs_f64();
            http::record_duration(elapsed, &method, scheme, status, &path, None);
            drop(permit);
            return response;
        }

        let path = req.uri().path().to_owned();
        let inbound = inbound_from_hyper(req, self.server_addr, self.client_addr);

        // Timeout-wrapped dispatch.
        let result = tokio::time::timeout(self.timeout, self.dispatch.dispatch(inbound)).await;

        let response = match result {
            Ok(mut outbound) => {
                // For streaming, hold the permit for the stream's lifetime.
                if let ResponseBody::Stream(stream) = outbound.body {
                    outbound.body = ResponseBody::Stream(Box::pin(PermitGuardedStream {
                        inner: stream,
                        _permit: permit,
                    }));
                } else {
                    drop(permit);
                }
                outbound_to_hyper(outbound)
            }
            Err(_elapsed) => {
                drop(permit);
                error_response(hyper::StatusCode::REQUEST_TIMEOUT, "request timeout")
            }
        };

        let status = response.status().as_u16();
        let elapsed = start.elapsed().as_secs_f64();
        let error_type = if status >= 400 {
            Some(status.to_string())
        } else {
            None
        };
        http::record_duration(
            elapsed,
            &method,
            scheme,
            status,
            &path,
            error_type.as_deref(),
        );

        response
    }
}

/// Check if the path is a health probe and return the response.
fn probe_response(path: &str) -> Option<Response<ResponseBody>> {
    let body = match path {
        "/healthz" => HEALTH_ALIVE,
        "/readyz" => HEALTH_READY,
        _ => return None,
    };

    // Builder with static status + header cannot fail.
    let resp = Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, JSON_CONTENT_TYPE)
        .body(ResponseBody::Fixed(Bytes::from_static(body)))
        .unwrap_or_else(|_| unreachable!());

    Some(resp)
}

/// Convert a hyper request to an `InboundRequest`.
fn inbound_from_hyper(
    req: Request<Incoming>,
    server_addr: SocketAddr,
    client_addr: Option<SocketAddr>,
) -> InboundRequest {
    let (parts, body) = req.into_parts();

    let method = parts.method;
    let path = parts.uri.path().to_owned();
    let query_string = parts
        .uri
        .query()
        .map(|q| Bytes::copy_from_slice(q.as_bytes()))
        .unwrap_or_default();
    let headers = parts.headers;

    let protocol = match parts.version {
        hyper::Version::HTTP_10 => ProtocolVersion::Http10,
        hyper::Version::HTTP_2 => ProtocolVersion::H2,
        _ => ProtocolVersion::Http11,
    };

    // Wrap Incoming as a BodyStream::Stream via http_body_util::BodyStream.
    let stream = http_body_util::BodyStream::new(body);
    let mapped = futures_util::StreamExt::map(stream, |result| {
        result
            .map(|frame| frame.into_data().unwrap_or_default())
            .map_err(|e| std::io::Error::other(e.to_string()))
    });
    let body_stream = BodyStream::Stream(Box::pin(mapped));

    InboundRequest::new(
        method,
        path,
        query_string,
        headers,
        body_stream,
        protocol,
        TransportKind::Tcp,
        client_addr,
        server_addr,
        Vec::new(),
        parts.extensions,
    )
}

/// Convert an `OutboundResponse` to a hyper response.
fn outbound_to_hyper(resp: OutboundResponse) -> Response<ResponseBody> {
    let mut builder = Response::builder().status(resp.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = resp.headers;
    }
    // Builder with valid status cannot fail.
    builder.body(resp.body).unwrap_or_else(|_| unreachable!())
}

/// Construct an error response with a plain-text body.
fn error_response(status: hyper::StatusCode, body: &str) -> Response<ResponseBody> {
    // Builder with valid status + header cannot fail.
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain")
        .body(ResponseBody::Fixed(Bytes::copy_from_slice(body.as_bytes())))
        .unwrap_or_else(|_| unreachable!())
}

// ── PermitGuardedStream ──────────────────────────────────────────────────

/// Streaming body that holds a semaphore permit for its lifetime.
///
/// Ensures SSE connections count against the concurrency limit
/// until the stream ends or the client disconnects.
struct PermitGuardedStream {
    inner: Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    _permit: OwnedSemaphorePermit,
}

impl futures_core::Stream for PermitGuardedStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

// ── Serve loop ───────────────────────────────────────────────────────────

/// Accept connections and serve them using the given `ApxService`.
///
/// Runs until the `shutdown` future completes, then stops accepting new
/// connections. In-flight connections continue until they complete naturally.
pub async fn serve_tcp(
    listener: TcpListener,
    service: ApxService,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, client_addr) = result?;
                let svc = service.clone().with_client_addr(client_addr);
                tokio::spawn(serve_connection(stream, svc));
            }
            () = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping accept loop");
                break;
            }
        }
    }

    Ok(())
}

/// Serve a single connection using HTTP/1 auto-detection.
async fn serve_connection(stream: tokio::net::TcpStream, service: ApxService) {
    let io = TokioIo::new(stream);
    let result = http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades() // Required for HTTP upgrade (WebSocket) protocol.
        .await;
    if let Err(e) = result {
        tracing::debug!(error = %e, "connection error");
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use ::http::header::HeaderMap;

    /// Stub dispatch for testing — returns 200 with "ok" body.
    #[derive(Debug)]
    struct StubDispatch;

    impl Dispatch for StubDispatch {
        fn dispatch(
            &self,
            _request: InboundRequest,
        ) -> Pin<Box<dyn Future<Output = OutboundResponse> + Send>> {
            Box::pin(async {
                OutboundResponse {
                    status: hyper::StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: ResponseBody::Fixed(Bytes::from_static(b"ok")),
                }
            })
        }
    }

    fn stub_service() -> ApxService {
        let dispatch: Arc<dyn Dispatch> = Arc::new(StubDispatch);
        let config = ServiceConfig::default();
        let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
        ApxService::new(dispatch, addr, &config)
    }

    #[test]
    fn probe_healthz_returns_200_with_json() {
        let resp = probe_response("/healthz").unwrap();
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            JSON_CONTENT_TYPE
        );
        match resp.body() {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), HEALTH_ALIVE),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn probe_readyz_returns_200_with_json() {
        let resp = probe_response("/readyz").unwrap();
        assert_eq!(resp.status(), hyper::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            JSON_CONTENT_TYPE
        );
        match resp.body() {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), HEALTH_READY),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn probe_unknown_path_returns_none() {
        assert!(probe_response("/api/users").is_none());
        assert!(probe_response("/").is_none());
        assert!(probe_response("/health").is_none());
    }

    #[test]
    fn service_config_default_values() {
        let config = ServiceConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_concurrent, 256);
    }

    #[test]
    fn error_response_503_service_unavailable() {
        let resp = error_response(hyper::StatusCode::SERVICE_UNAVAILABLE, "overloaded");
        assert_eq!(resp.status(), hyper::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        match resp.body() {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"overloaded"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn error_response_408_request_timeout() {
        let resp = error_response(hyper::StatusCode::REQUEST_TIMEOUT, "timeout");
        assert_eq!(resp.status(), hyper::StatusCode::REQUEST_TIMEOUT);
        match resp.body() {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"timeout"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn outbound_to_hyper_preserves_status_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let outbound = OutboundResponse {
            status: hyper::StatusCode::CREATED,
            headers,
            body: ResponseBody::Fixed(Bytes::from_static(b"{}")),
        };

        let resp = outbound_to_hyper(outbound);
        assert_eq!(resp.status(), hyper::StatusCode::CREATED);
        assert_eq!(resp.headers().get("x-custom").unwrap(), "value");
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn apx_service_debug_does_not_panic() {
        let service = stub_service();
        let dbg = format!("{service:?}");
        assert!(dbg.contains("ApxService"));
        assert!(dbg.contains("8080"));
    }

    #[tokio::test]
    async fn permit_guarded_stream_holds_permit() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&sem).try_acquire_owned().unwrap();

        // Wrap a stream with the permit.
        let chunks = vec![Ok(Bytes::from("hello")), Ok(Bytes::from(" world"))];
        let inner_stream = tokio_stream::iter(chunks);
        let mut stream = PermitGuardedStream {
            inner: Box::pin(inner_stream),
            _permit: permit,
        };

        // While the stream is alive, the semaphore should have 0 permits.
        assert_eq!(sem.available_permits(), 0);

        // Consume the stream.
        use futures_core::Stream;
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = Pin::new(&mut stream).poll_next(&mut cx);
        assert_eq!(sem.available_permits(), 0);

        drop(stream);
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn fixed_response_drops_permit_immediately() {
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::clone(&sem).try_acquire_owned().unwrap();
        assert_eq!(sem.available_permits(), 0);

        // Simulate what handle() does for fixed responses.
        let outbound = OutboundResponse {
            status: hyper::StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBody::Fixed(Bytes::from_static(b"ok")),
        };

        // Fixed body — permit is dropped immediately.
        if matches!(outbound.body, ResponseBody::Stream(_)) {
            unreachable!();
        } else {
            drop(permit);
        }
        assert_eq!(sem.available_permits(), 1);
    }
}
