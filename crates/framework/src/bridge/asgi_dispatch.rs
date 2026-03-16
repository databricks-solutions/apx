//! ASGI dispatch — calls `app(scope, receive, send)` and collects response.
//!
//! Implements the [`Dispatch`] trait for ASGI applications. The hot path
//! collects the request body, then builds scope + calls app + drives the
//! coroutine inline on the tokio thread via the Rust scheduler.
//!
//! The response flows through a oneshot channel: `AsgiSend` accumulates
//! status/headers from `ResponseStart` and builds the complete
//! `OutboundResponse` on the first body chunk.

use crate::bridge::asgi::{AsgiReceive, AsgiSend, ScopeInterns, build_http_scope};
use crate::dispatch::Dispatch;
use crate::error::AppError;
use crate::scheduler::driver::spawn_and_drive;
use crate::transport::types::{BodyStream, InboundRequest, OutboundResponse, ResponseBody};
use crate::worker_context::WorkerContext;
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
    /// Template dict for `AsgiReceive` (Arc-wrapped to avoid per-request GIL).
    receive_template: Arc<Py<PyDict>>,
    /// Shared worker infrastructure (scheduler state).
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
        ctx: Arc<WorkerContext>,
        body_limit: usize,
    ) -> Self {
        Self {
            app: Arc::new(app),
            scope_interns,
            receive_template: Arc::new(receive_template),
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
        // Take body before moving request into the async block.
        let body_stream = request.take_body();
        let body_limit = self.body_limit;

        // Arc clones — no GIL needed on the tokio thread.
        let app = Arc::clone(&self.app);
        let template = Arc::clone(&self.receive_template);
        let interns = Arc::clone(&self.scope_interns);
        let ctx = Arc::clone(&self.ctx);

        Box::pin(async move {
            match dispatch_inner(
                request,
                body_stream,
                body_limit,
                app,
                interns,
                template,
                ctx,
            )
            .await
            {
                Ok(resp) => resp,
                Err(err) => error_response(err),
            }
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
            match crate::websocket::handle_upgrade(
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

/// Full dispatch pipeline: collect body → build scope → call app → drive
/// coroutine inline → await oneshot response.
async fn dispatch_inner(
    request: InboundRequest,
    body_stream: BodyStream,
    body_limit: usize,
    app: Arc<Py<PyAny>>,
    interns: Arc<ScopeInterns>,
    template: Arc<Py<PyDict>>,
    ctx: Arc<WorkerContext>,
) -> Result<OutboundResponse, AppError> {
    // Step 1: Collect body (async, no GIL).
    let body_bytes = body_stream
        .collect(body_limit)
        .await
        .map_err(|e| AppError::Internal(format!("body collect: {e}")))?;

    // Step 2: Create oneshot response channel + disconnect channel.
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel::<()>();

    // Step 3: Build scope, call app, drive coroutine — all inline.
    Python::attach(|py| -> Result<(), AppError> {
        let scope = build_http_scope(py, &request, None, &interns)
            .map_err(|e| AppError::Internal(format!("scope build: {e}")))?;
        let tmpl = (*template).clone_ref(py);
        let receive = if body_bytes.is_empty() {
            AsgiReceive::empty(tmpl, disconnect_rx)
        } else {
            AsgiReceive::http(body_bytes, tmpl, disconnect_rx)
        };
        let receive_obj =
            Py::new(py, receive).map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
        let send = AsgiSend::http(response_tx, disconnect_tx);
        let send_obj =
            Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;
        let coro = app
            .call1(py, (scope, receive_obj, send_obj))
            .map_err(|e| AppError::Internal(format!("ASGI app call: {e}")))?;

        // Drive coroutine inline via spawn_and_drive.
        // For simple handlers: completes here, response sent via oneshot.
        // For suspending handlers: ResumeCallback enqueued to ready_queue.
        let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
        spawn_and_drive(
            py,
            coro,
            result_tx,
            &ctx.cached_types,
            &ctx.call_soon,
            &ctx.ready_queue,
        );
        Ok(())
    })?;

    // Step 4: Await response from the oneshot channel.
    response_rx
        .await
        .map_err(|_| AppError::Internal("response channel closed".to_owned()))?
}

/// Map an [`AppError`] to a generic HTTP error response.
///
/// The error detail is logged but NOT leaked to the client.
fn error_response(err: AppError) -> OutboundResponse {
    let status = err.status_code();
    let body = match &err {
        AppError::Timeout => "request timeout",
        AppError::Internal(msg) => {
            tracing::error!(error = %msg, "internal dispatch error");
            "Internal Server Error"
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
        body: ResponseBody::Fixed(Bytes::copy_from_slice(body.as_bytes())),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::panic, reason = "test code uses unwrap/assert for clarity")]
mod tests {
    use super::*;

    // ── error_response tests ────────────────────────────────────────────

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
