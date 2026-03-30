//! ASGI dispatch — zero-GIL 3-thread HTTP dispatch + legacy WS dispatch.
//!
//! HTTP requests flow through the crossbeam pipeline:
//!   Thread 1 (tokio) → crossbeam → Thread 2 (asyncio) → crossbeam → Thread 3 → oneshot → Thread 1
//!
//! WebSocket upgrades still use the legacy `call_soon_threadsafe(launch_fn, ...)`
//! path until WS is migrated to crossbeam.

use crate::asgi::channel_body::ChannelBody;
use crate::asgi::scope::ScopeInterns;
use crate::dispatch::Dispatch;
use crate::io::channel::{RequestSlot, ResponseData, Wakeup};
use crate::protocol::http::error::AppError;
use crate::supervision::worker_context::WorkerContext;
use crate::telemetry::context::TraceContext;
use crate::telemetry::dispatch_metrics;
use crate::telemetry::timed;
use crate::transport::types::{BodyStream, InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::body::Incoming;
use hyper::{Request, Response};
use pyo3::prelude::*;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

// ── AsgiDispatch ─────────────────────────────────────────────────────────

/// ASGI dispatch: HTTP via crossbeam pipeline (no GIL), WS via legacy path.
pub struct AsgiDispatch {
    /// Inbound channel sender — pushes `RequestSlot` to Thread 2.
    inbound_tx: crossbeam_channel::Sender<RequestSlot>,
    /// Wakeup signal for the asyncio thread.
    wakeup: Arc<Wakeup>,
    /// Maximum request body size in bytes.
    body_limit: usize,

    // ── WS legacy fields (until WS migrates to crossbeam) ──
    /// The Python ASGI callable.
    app: Arc<Py<PyAny>>,
    /// Pre-interned scope strings (shared with RequestQueue on Thread 2).
    scope_interns: Arc<ScopeInterns>,
    /// Shared worker context (carries call_soon_threadsafe + launch_fn for WS).
    ctx: Arc<WorkerContext>,
}

impl AsgiDispatch {
    /// Create a new `AsgiDispatch` with crossbeam pipeline for HTTP.
    pub fn new(
        inbound_tx: crossbeam_channel::Sender<RequestSlot>,
        wakeup: Arc<Wakeup>,
        body_limit: usize,
        app: Py<PyAny>,
        scope_interns: Arc<ScopeInterns>,
        ctx: Arc<WorkerContext>,
    ) -> Self {
        Self {
            inbound_tx,
            wakeup,
            body_limit,
            app: Arc::new(app),
            scope_interns,
            ctx,
        }
    }
}

impl std::fmt::Debug for AsgiDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiDispatch")
            .field("body_limit", &self.body_limit)
            .finish_non_exhaustive()
    }
}

impl Dispatch for AsgiDispatch {
    fn dispatch(
        &self,
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = OutboundResponse> + Send>> {
        let body_stream = request.take_body();
        let body_limit = self.body_limit;
        let inbound_tx = self.inbound_tx.clone();
        let wakeup = Arc::clone(&self.wakeup);

        Box::pin(async move {
            let result = dispatch_inner(request, body_stream, body_limit, inbound_tx, wakeup).await;
            result.unwrap_or_else(error_response)
        })
    }

    fn dispatch_ws(
        &self,
        request: Request<Incoming>,
        server_addr: SocketAddr,
        client_addr: Option<SocketAddr>,
    ) -> Pin<Box<dyn Future<Output = Response<ResponseBody>> + Send>> {
        let app = Arc::clone(&self.app);
        let interns = Arc::clone(&self.scope_interns);
        let ctx = Arc::clone(&self.ctx);

        Box::pin(async move {
            match crate::protocol::ws::session::handle_upgrade(
                request,
                server_addr,
                client_addr,
                app,
                interns,
                ctx,
            ) {
                Ok(response) => response,
                Err(err) => {
                    tracing::error!(name: "apx.dispatch.websocket_upgrade_error", error = %err, "websocket upgrade error");
                    Response::builder()
                        .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                        .header(http::header::CONTENT_TYPE, "text/plain")
                        .body(ResponseBody::Fixed(Bytes::from_static(
                            b"Internal Server Error",
                        )))
                        .unwrap_or_else(|_| unreachable!())
                }
            }
        })
    }
}

// ── Dispatch internals ───────────────────────────────────────────────────

/// Zero-GIL HTTP dispatch: extract trace context, then time the full pipeline.
async fn dispatch_inner(
    request: InboundRequest,
    body_stream: BodyStream,
    body_limit: usize,
    inbound_tx: crossbeam_channel::Sender<RequestSlot>,
    wakeup: Arc<Wakeup>,
) -> Result<OutboundResponse, AppError> {
    if let Some(id) = request
        .headers
        .get(&crate::protocol::http::service::REQUEST_ID_HEADER)
        && let Ok(val) = id.to_str()
    {
        tracing::Span::current().record("request.id", val);
    }

    let trace_context = crate::telemetry::context::extract_trace_context();

    timed!(
        dispatch_metrics::record_dispatch_total,
        dispatch_pipeline(
            request,
            body_stream,
            body_limit,
            inbound_tx,
            wakeup,
            trace_context
        )
        .await
    )
}

/// Collect body → build RequestSlot → push to crossbeam → await response.
async fn dispatch_pipeline(
    request: InboundRequest,
    body_stream: BodyStream,
    body_limit: usize,
    inbound_tx: crossbeam_channel::Sender<RequestSlot>,
    wakeup: Arc<Wakeup>,
    trace_context: Option<TraceContext>,
) -> Result<OutboundResponse, AppError> {
    let body_bytes = timed!(
        dispatch_metrics::record_body_collect,
        body_stream
            .collect(body_limit)
            .await
            .map_err(|e| AppError::Internal(format!("body collect: {e}")))?
    );

    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    let raw_path = Bytes::copy_from_slice(request.path.as_bytes());
    let slot = RequestSlot {
        method: request.method.clone(),
        path: request.path.clone(),
        raw_path,
        query_string: request.query_string.clone(),
        headers: request.headers.clone(),
        body: body_bytes,
        protocol: request.protocol,
        client_addr: request.client_addr,
        server_addr: request.server_addr,
        trace_context,
        created_at: std::time::Instant::now(),
        response_tx,
    };

    timed!(dispatch_metrics::record_crossbeam_send, {
        inbound_tx
            .send(slot)
            .map_err(|_| AppError::Internal("inbound channel closed".to_owned()))?;
        wakeup.signal();
    });

    let response_data = timed!(
        dispatch_metrics::record_response_wait,
        response_rx
            .await
            .map_err(|_| AppError::Internal("response channel closed".to_owned()))?
    );

    response_data_to_outbound(response_data)
}

/// Convert a `ResponseData` from the crossbeam pipeline into an `OutboundResponse`.
fn response_data_to_outbound(data: ResponseData) -> Result<OutboundResponse, AppError> {
    let status =
        http::StatusCode::from_u16(data.status).unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
    let mut headers = HeaderMap::with_capacity(data.headers.len());
    for (name, value) in &data.headers {
        let header_name = HeaderName::from_bytes(name)
            .map_err(|e| AppError::Internal(format!("invalid header name: {e}")))?;
        let header_value = HeaderValue::from_bytes(value)
            .map_err(|e| AppError::Internal(format!("invalid header value: {e}")))?;
        headers.append(header_name, header_value);
    }
    let body = ResponseBody::Stream(Box::pin(ChannelBody::new(data.body_rx)));

    Ok(OutboundResponse {
        status,
        headers,
        body,
        server_route: None,
    })
}

/// Client-visible body for internal errors.
const INTERNAL_ERROR_BODY: &[u8] = b"Internal Server Error";

/// Client-visible body for request timeout.
const TIMEOUT_BODY: &[u8] = b"request timeout";

/// Map an [`AppError`] to a generic HTTP error response.
///
/// The error detail is logged but NOT leaked to the client.
fn error_response(err: AppError) -> OutboundResponse {
    let status = err.status_code();
    let body = match &err {
        AppError::Timeout => TIMEOUT_BODY,
        AppError::Internal(msg) => {
            tracing::error!(name: "apx.dispatch.internal_error", error = %msg, "internal dispatch error");
            INTERNAL_ERROR_BODY
        }
    };
    OutboundResponse {
        status,
        headers: {
            let mut h = HeaderMap::new();
            h.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain"),
            );
            h
        },
        body: ResponseBody::Fixed(Bytes::from_static(body)),
        server_route: None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::panic, reason = "test code uses unwrap/assert for clarity")]
mod tests {
    use super::*;

    #[test]
    fn error_response_internal() {
        let err = AppError::Internal("db connection failed".to_owned());
        let resp = error_response(err);
        assert_eq!(resp.status, http::StatusCode::INTERNAL_SERVER_ERROR);
        match &resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"Internal Server Error"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn error_response_timeout() {
        let err = AppError::Timeout;
        let resp = error_response(err);
        assert_eq!(resp.status, http::StatusCode::REQUEST_TIMEOUT);
        match &resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"request timeout"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }
}
