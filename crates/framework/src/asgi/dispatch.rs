//! ASGI dispatch — calls `app(scope, receive, send)` and collects response.
//!
//! Implements the [`Dispatch`] trait for ASGI applications. The hot path
//! collects the request body, builds scope, wraps the ASGI coroutine in
//! `_guarded` (which catches `Exception` and forwards it as a 500 without
//! re-raising), and submits it to asyncio via `call_soon_threadsafe(create_task, ...)`.
//!
//! The response flows through a oneshot channel: `AsgiSend` accumulates
//! status/headers from `ResponseStart` and builds the complete
//! `OutboundResponse` on the first body chunk.

use crate::asgi::scope::{AsgiReceive, AsgiSend, ScopeInterns, SendCache, scope_from_template};
use crate::dispatch::Dispatch;
use crate::protocol::http::error::AppError;
use crate::supervision::worker_context::WorkerContext;
use crate::transport::types::{BodyStream, InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::header::HeaderMap;
use hyper::body::Incoming;
use hyper::{Request, Response};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

// ── AsgiDispatch ─────────────────────────────────────────────────────────

/// ASGI dispatch: calls `app(scope, receive, send)` with oneshot response.
///
/// `AsgiSend` accumulates status/headers from `ResponseStart` and sends the
/// complete `OutboundResponse` via a oneshot channel on the first body chunk.
pub struct AsgiDispatch {
    /// The Python ASGI callable (Arc-wrapped to avoid per-request GIL).
    app: Arc<Py<PyAny>>,
    /// Pre-interned scope strings, shared across all requests.
    scope_interns: Arc<ScopeInterns>,
    /// Pre-built receive dict template, cloned per-request via `PyDict::copy`.
    receive_template: Arc<Py<PyDict>>,
    /// Cached Python objects for the send path.
    send_cache: Arc<SendCache>,
    /// Shared worker infrastructure (asyncio submission state).
    ctx: Arc<WorkerContext>,
    /// Maximum request body size in bytes.
    body_limit: usize,
}

impl AsgiDispatch {
    /// Create a new `AsgiDispatch`.
    pub fn new(
        app: Py<PyAny>,
        scope_interns: Arc<ScopeInterns>,
        receive_template: Py<PyDict>,
        send_cache: Arc<SendCache>,
        ctx: Arc<WorkerContext>,
        body_limit: usize,
    ) -> Self {
        Self {
            app: Arc::new(app),
            scope_interns,
            receive_template: Arc::new(receive_template),
            send_cache,
            ctx,
            body_limit,
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

        let app = Arc::clone(&self.app);
        let interns = Arc::clone(&self.scope_interns);
        let recv_tpl = Arc::clone(&self.receive_template);
        let send_cache = Arc::clone(&self.send_cache);
        let ctx = Arc::clone(&self.ctx);

        Box::pin(async move {
            let result = dispatch_inner(
                request,
                body_stream,
                body_limit,
                app,
                interns,
                recv_tpl,
                send_cache,
                ctx,
            )
            .await;
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
                    tracing::error!(error = %err, "websocket upgrade error");
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

/// Full dispatch pipeline: collect body → build scope → submit to asyncio
/// → await oneshot response.
///
/// When `APX_PERF=1`, emits per-phase timing events under the `apx.perf`
/// tracing target. Zero overhead when disabled.
#[expect(clippy::too_many_arguments, reason = "dispatch args are all required")]
async fn dispatch_inner(
    request: InboundRequest,
    body_stream: BodyStream,
    body_limit: usize,
    app: Arc<Py<PyAny>>,
    interns: Arc<ScopeInterns>,
    recv_tpl: Arc<Py<PyDict>>,
    send_cache: Arc<SendCache>,
    ctx: Arc<WorkerContext>,
) -> Result<OutboundResponse, AppError> {
    if let Some(id) = request
        .headers
        .get(&crate::protocol::http::service::REQUEST_ID_HEADER)
        && let Ok(val) = id.to_str()
    {
        tracing::Span::current().record("request.id", val);
    }

    let perf = crate::telemetry::perf_enabled();
    let t_total = perf.then(Instant::now);

    let t0 = perf.then(Instant::now);
    let body_bytes = body_stream
        .collect(body_limit)
        .await
        .map_err(|e| AppError::Internal(format!("body collect: {e}")))?;
    if let Some(t) = t0 {
        tracing::info!(target: "apx.perf", phase = "body_collect", elapsed_us = t.elapsed().as_micros() as u64);
    }

    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel::<()>();

    let t_gil = perf.then(Instant::now);
    Python::attach(|py| -> Result<(), AppError> {
        if let Some(t) = t_gil {
            tracing::info!(target: "apx.perf", phase = "gil_acquire", elapsed_us = t.elapsed().as_micros() as u64);
        }

        let t0 = perf.then(Instant::now);
        let scope = scope_from_template(py, &interns.scope_template, &request, None, &interns)
            .map_err(|e| AppError::Internal(format!("scope build: {e}")))?;
        if let Some(t) = t0 {
            tracing::info!(target: "apx.perf", phase = "scope_build", elapsed_us = t.elapsed().as_micros() as u64);
        }

        let tpl = recv_tpl.clone_ref(py);
        let receive = if body_bytes.is_empty() {
            AsgiReceive::empty(disconnect_rx, tpl)
        } else {
            AsgiReceive::http(body_bytes, disconnect_rx, tpl)
        };
        let receive_obj =
            Py::new(py, receive).map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
        let send = AsgiSend::http(response_tx, disconnect_tx, &send_cache, py);
        let send_obj =
            Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;

        if let Some(trace_ctx) = crate::telemetry::context::extract_trace_context() {
            let _ = crate::telemetry::context::set_python_context(py, &trace_ctx);
        }

        let t0 = perf.then(Instant::now);
        ctx.call_soon_threadsafe
            .call1(py, (&ctx.launch_fn, &*app, &scope, &receive_obj, &send_obj))
            .map_err(|e| AppError::Internal(format!("submit to asyncio: {e}")))?;
        if let Some(t) = t0 {
            tracing::info!(target: "apx.perf", phase = "submit", elapsed_us = t.elapsed().as_micros() as u64);
        }

        Ok(())
    })?;

    let t0 = perf.then(Instant::now);
    let response = response_rx
        .await
        .map_err(|_| AppError::Internal("response channel closed".to_owned()))?;
    if let Some(t) = t0 {
        tracing::info!(target: "apx.perf", phase = "response_wait", elapsed_us = t.elapsed().as_micros() as u64);
    }

    if let Some(t) = t_total {
        let status = response.as_ref().map(|r| r.status.as_u16()).unwrap_or(500);
        tracing::info!(
            target: "apx.perf",
            phase = "dispatch",
            elapsed_us = t.elapsed().as_micros() as u64,
            status,
            method = %request.method,
            path = %request.path,
        );
    }

    response
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
            tracing::error!(error = %msg, "internal dispatch error");
            INTERNAL_ERROR_BODY
        }
    };
    OutboundResponse {
        status,
        headers: {
            let mut h = HeaderMap::new();
            h.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("text/plain"),
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
