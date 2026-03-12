//! ASGI bridge dispatch — runs handlers through scope/receive/send.
//!
//! This dispatch path is used for routes with `Depends()`, streaming
//! responses, or `Request`/`Response` parameter injection. It constructs
//! Rust-backed ASGI objects from [`InboundRequest`] and collects the
//! response from the ASGI send channel.

use crate::bridge::asgi::{AsgiEvent, AsgiEventBuffer, AsgiReceive, AsgiSend};
use crate::bridge::bench_trace_enabled;
use crate::bridge::context_pool::scope_from_template;
use crate::bridge::dispatch::{AppState, HandlerDispatch};
use crate::error::{AppError, BodyParseKind};
use crate::route::{BoundRoute, HandlerKind, ResponseType};
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::StatusCode;
use pyo3::prelude::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Channel buffer size for ASGI response events.
const ASGI_CHANNEL_SIZE: usize = 8;

/// Dispatch via the ASGI bridge (scope/receive/send).
pub struct AsgiBridgeDispatch;

impl std::fmt::Debug for AsgiBridgeDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiBridgeDispatch").finish()
    }
}

impl HandlerDispatch for AsgiBridgeDispatch {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>> {
        Box::pin(async move {
            tracing::debug!(
                path = %request.path,
                handler = %route.manifest.handler_qualname,
                "asgi_dispatch: handle entry"
            );

            // 1. Take body + collect bytes
            let body_stream = request.take_body();
            let body_bytes = body_stream
                .collect(app_state.max_body_limit.0)
                .await
                .map_err(|_| AppError::BodyParse(BodyParseKind::BodyTooLarge))?;

            // 2. Branch on streaming vs buffered before creating ASGI objects.
            //    Buffered path avoids mpsc channel and tokio::spawn entirely.
            let is_streaming = matches!(route.manifest.kind, HandlerKind::SSE)
                || matches!(
                    route.manifest.response_type,
                    ResponseType::StreamingResponse
                );

            if is_streaming {
                dispatch_streaming(route, app_state, request, body_bytes).await
            } else {
                dispatch_buffered(route, app_state, request, body_bytes).await
            }
        })
    }
}

/// Buffered response path: Granian-style, Python owns the coroutine lifecycle.
///
/// All Python work runs on the event loop thread via `schedule_callback`.
/// Rust only delivers the request and waits for the response through `send()`.
async fn dispatch_buffered(
    route: Arc<BoundRoute>,
    app_state: Arc<AppState>,
    request: InboundRequest,
    body_bytes: Bytes,
) -> Result<OutboundResponse, AppError> {
    let trace = bench_trace_enabled();
    let t_total = trace.then(std::time::Instant::now);

    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    // Only Arc clones (atomic increment) — no GIL needed on the tokio thread.
    let app_state_inner = Arc::clone(&app_state);

    // Extract trace context before crossing to the event loop thread.
    let trace_ctx = crate::telemetry::context::extract_trace_context();

    let t_schedule = trace.then(std::time::Instant::now);
    // Capture enqueue time to measure queue latency inside the callback.
    let t_enqueued = trace.then(std::time::Instant::now);
    app_state.loop_handle.schedule_callback(move |py| {
        let result = (|| -> Result<(), AppError> {
            if let Some(ref ctx) = trace_ctx {
                let _ = crate::telemetry::context::set_python_context(py, ctx);
            }
            let t_cb_start = t_enqueued.map(|t| t.elapsed().as_micros());

            let asgi_callable = route
                .fastapi_app
                .as_ref()
                .map_or_else(|| route.handler.inner(), |a| a.inner());

            let t_scope = trace.then(std::time::Instant::now);
            let scope = scope_from_template(
                py,
                &app_state_inner.scope_template,
                &request,
                &app_state_inner.scope_interns,
            )
            .map_err(|e| AppError::Internal(format!("build scope: {e}")))?;
            let scope_us = t_scope.map(|t| t.elapsed().as_micros());

            let t_objects = trace.then(std::time::Instant::now);
            let receive_template_ref = app_state_inner.receive_template.as_ref().clone_ref(py);
            let receive = AsgiReceive::http(body_bytes, receive_template_ref);
            let send = AsgiSend::response_driven(response_tx);

            let receive_obj = Py::new(py, receive)
                .map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
            let send_obj =
                Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;
            let objects_us = t_objects.map(|t| t.elapsed().as_micros());

            let t_call = trace.then(std::time::Instant::now);
            let coro = asgi_callable
                .call(py, (scope, receive_obj, send_obj), None)
                .map_err(|e| AppError::Internal(format!("handler call: {e}")))?;
            let call_us = t_call.map(|t| t.elapsed().as_micros());

            let t_task = trace.then(std::time::Instant::now);
            // Python owns the coroutine lifecycle — with eager task factory
            // (Python 3.12+), create_task runs the first step inline,
            // completing synchronous handlers without scheduling delay.
            let task = app_state_inner
                .create_task
                .call1(py, (coro,))
                .map_err(|e| AppError::Internal(format!("create_task: {e}")))?;

            let _ = task.call_method1(py, c"add_done_callback", (&app_state_inner.error_logger,));
            let task_us = t_task.map(|t| t.elapsed().as_micros());

            if trace {
                tracing::info!(
                    target: "bench_trace",
                    phase = "callback_inner",
                    queue_latency_us = t_cb_start.unwrap_or(0),
                    scope_us = scope_us.unwrap_or(0),
                    objects_us = objects_us.unwrap_or(0),
                    call_us = call_us.unwrap_or(0),
                    create_task_us = task_us.unwrap_or(0),
                );
            }

            Ok(())
        })();

        if let Err(e) = result {
            tracing::error!(error = %e, "ASGI dispatch setup failed");
            // response_tx was moved into AsgiSend; if we errored before that,
            // it's still in scope and will drop, causing RecvError on response_rx.
        }
    })?;
    let schedule_us = t_schedule.map(|t| t.elapsed().as_micros());

    // Wait for send() to deliver the complete response.
    let t_await = trace.then(std::time::Instant::now);
    let response = response_rx.await.map_err(|_| {
        AppError::Internal("ASGI handler failed before sending response".to_owned())
    })??;
    let await_us = t_await.map(|t| t.elapsed().as_micros());

    if let Some(t_total) = t_total {
        tracing::info!(
            target: "bench_trace",
            phase = "dispatch_buffered",
            total_us = t_total.elapsed().as_micros(),
            schedule_us = schedule_us.unwrap_or(0),
            await_us = await_us.unwrap_or(0),
        );
    }

    Ok(response)
}

/// Streaming response path: Granian-style, Python owns the coroutine lifecycle.
///
/// Uses mpsc channel for concurrent body streaming. The event loop thread
/// creates the task; channel closing handles cleanup when the handler finishes.
async fn dispatch_streaming(
    route: Arc<BoundRoute>,
    app_state: Arc<AppState>,
    request: InboundRequest,
    body_bytes: Bytes,
) -> Result<OutboundResponse, AppError> {
    let (send_tx, send_rx) = mpsc::channel::<AsgiEvent>(ASGI_CHANNEL_SIZE);
    // Only Arc clone — no GIL needed on the tokio thread.
    let app_state_inner = Arc::clone(&app_state);

    // Extract trace context before crossing to the event loop thread.
    let trace_ctx = crate::telemetry::context::extract_trace_context();

    app_state.loop_handle.schedule_callback(move |py| {
        let result = (|| -> Result<(), AppError> {
            if let Some(ref ctx) = trace_ctx {
                let _ = crate::telemetry::context::set_python_context(py, ctx);
            }
            let asgi_callable = route
                .fastapi_app
                .as_ref()
                .map_or_else(|| route.handler.inner(), |a| a.inner());

            let scope = scope_from_template(
                py,
                &app_state_inner.scope_template,
                &request,
                &app_state_inner.scope_interns,
            )
            .map_err(|e| AppError::Internal(format!("build scope: {e}")))?;

            let receive_template_ref = app_state_inner.receive_template.as_ref().clone_ref(py);
            let receive = if body_bytes.is_empty() {
                AsgiReceive::empty(receive_template_ref)
            } else {
                AsgiReceive::http(body_bytes, receive_template_ref)
            };
            let send = AsgiSend::channel(send_tx);

            let receive_obj = Py::new(py, receive)
                .map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
            let send_obj =
                Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;

            let coro = asgi_callable
                .call(py, (scope, receive_obj, send_obj), None)
                .map_err(|e| AppError::Internal(format!("handler call: {e}")))?;

            let task = app_state_inner
                .create_task
                .call1(py, (coro,))
                .map_err(|e| AppError::Internal(format!("create_task: {e}")))?;

            let _ = task.call_method1(py, c"add_done_callback", (&app_state_inner.error_logger,));

            Ok(())
        })();

        if let Err(e) = result {
            tracing::error!(error = %e, "ASGI streaming dispatch setup failed");
        }
    })?;

    // No handler_task needed — channel closing handles cleanup.
    super::streaming::stream_asgi_response_no_task(send_rx).await
}

/// Build ASGI scope, receive, and send for the buffered dispatch path.
///
/// Uses `AsgiReceive::http` (synchronous first-call body delivery)
/// and `AsgiSend::buffered` for zero-overhead send collection.
#[expect(
    dead_code,
    reason = "asgi refactor: replaced by inline closure in dispatch_buffered"
)]
fn call_asgi_app_sync(
    py: Python<'_>,
    route: &BoundRoute,
    request: &InboundRequest,
    app_state: &AppState,
    body_bytes: Bytes,
    event_buffer: &AsgiEventBuffer,
) -> Result<Py<PyAny>, AppError> {
    let trace = bench_trace_enabled();
    let t_total = trace.then(std::time::Instant::now);

    let asgi_callable = route
        .fastapi_app
        .as_ref()
        .map_or_else(|| route.handler.inner(), |a| a.inner());

    let t_scope = trace.then(std::time::Instant::now);
    let scope = scope_from_template(
        py,
        &app_state.scope_template,
        request,
        &app_state.scope_interns,
    )
    .map_err(|e| AppError::Internal(format!("build scope: {e}")))?;
    let scope_us = t_scope.map(|t| t.elapsed().as_micros());

    let t_objects = trace.then(std::time::Instant::now);
    let receive_template = app_state.receive_template.clone_ref(py);
    let receive = AsgiReceive::http(body_bytes, receive_template);
    let send = AsgiSend::buffered(event_buffer);

    let receive_obj =
        Py::new(py, receive).map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
    let send_obj = Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;
    let objects_us = t_objects.map(|t| t.elapsed().as_micros());

    let t_call = trace.then(std::time::Instant::now);
    let result = asgi_callable
        .call(py, (scope, receive_obj, send_obj), None)
        .map_err(|e| AppError::Internal(format!("handler call: {e}")))?;
    let call_us = t_call.map(|t| t.elapsed().as_micros());

    if let Some(t_total) = t_total {
        tracing::info!(
            target: "bench_trace",
            phase = "call_asgi_app_sync",
            total_us = t_total.elapsed().as_micros(),
            scope_us = scope_us.unwrap_or(0),
            objects_us = objects_us.unwrap_or(0),
            call_us = call_us.unwrap_or(0),
        );
    }

    Ok(result)
}

/// Build ASGI scope, receive, and send objects, then call the ASGI app.
///
/// Used by the streaming dispatch path. The `make_send` closure creates
/// the appropriate `AsgiSend` variant.
#[expect(
    dead_code,
    reason = "asgi refactor: replaced by inline closure in dispatch_streaming"
)]
fn call_asgi_app(
    py: Python<'_>,
    route: &BoundRoute,
    request: &InboundRequest,
    app_state: &AppState,
    body_bytes: Bytes,
    make_send: impl FnOnce(Python<'_>) -> AsgiSend,
) -> Result<Py<PyAny>, AppError> {
    let asgi_callable = route
        .fastapi_app
        .as_ref()
        .map_or_else(|| route.handler.inner(), |a| a.inner());

    let scope = scope_from_template(
        py,
        &app_state.scope_template,
        request,
        &app_state.scope_interns,
    )
    .map_err(|e| AppError::Internal(format!("build scope: {e}")))?;

    let receive_template = app_state.receive_template.clone_ref(py);
    let receive = if body_bytes.is_empty() {
        AsgiReceive::empty(receive_template)
    } else {
        AsgiReceive::http(body_bytes, receive_template)
    };
    let send = make_send(py);

    let receive_obj =
        Py::new(py, receive).map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
    let send_obj = Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;

    asgi_callable
        .call(py, (scope, receive_obj, send_obj), None)
        .map_err(|e| AppError::Internal(format!("handler call: {e}")))
}

/// Collect a buffered response from the event buffer.
///
/// Expects exactly one `ResponseStart` followed by one or more `ResponseBody` events.
#[expect(
    dead_code,
    reason = "asgi refactor: replaced by ResponseDriven send backend"
)]
fn collect_buffered_response(buffer: AsgiEventBuffer) -> Result<OutboundResponse, AppError> {
    let events = buffer.take();
    let mut events_iter = events.into_iter();

    let (status, headers) = match events_iter.next() {
        Some(AsgiEvent::ResponseStart { status, headers }) => (status, headers),
        Some(_) => {
            return Err(AppError::Internal(
                "ASGI protocol error: first event is not response start".to_owned(),
            ));
        }
        None => {
            return Err(AppError::Internal(
                "ASGI protocol error: no events in buffer".to_owned(),
            ));
        }
    };

    let body = collect_body_from_iter(&mut events_iter)?;

    Ok(OutboundResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        headers,
        body: ResponseBody::Fixed(body),
    })
}

/// Collect body bytes from an iterator of ASGI events.
fn collect_body_from_iter(events: &mut impl Iterator<Item = AsgiEvent>) -> Result<Bytes, AppError> {
    match events.next() {
        Some(AsgiEvent::ResponseBody { body, more_body }) if !more_body => Ok(body),
        Some(AsgiEvent::ResponseBody { body, .. }) => {
            let mut buf = Vec::from(body.as_ref());
            for event in events {
                if let AsgiEvent::ResponseBody { body, more_body } = event {
                    buf.extend_from_slice(&body);
                    if !more_body {
                        break;
                    }
                }
            }
            Ok(Bytes::from(buf))
        }
        Some(_) => Err(AppError::Internal(
            "ASGI protocol error: expected response body".to_owned(),
        )),
        None => Ok(Bytes::new()),
    }
}

/// Receive the `ResponseStart` event from the channel.
pub(super) async fn recv_response_start(
    rx: &mut mpsc::Receiver<AsgiEvent>,
) -> Result<(u16, http::header::HeaderMap), AppError> {
    match rx.recv().await {
        Some(AsgiEvent::ResponseStart { status, headers }) => Ok((status, headers)),
        Some(AsgiEvent::ResponseBody { .. }) => Err(AppError::Internal(
            "ASGI protocol error: received body before response start".to_owned(),
        )),
        Some(_) => Err(AppError::Internal(
            "ASGI protocol error: unexpected event before response start".to_owned(),
        )),
        None => Err(AppError::Internal(
            "ASGI protocol error: channel closed before response start".to_owned(),
        )),
    }
}

/// Channel-based response collection — used by tests.
#[cfg(test)]
async fn collect_asgi_response(
    rx: &mut mpsc::Receiver<AsgiEvent>,
) -> Result<OutboundResponse, AppError> {
    let (status, headers) = recv_response_start(rx).await?;
    let body = recv_response_body(rx).await?;
    Ok(OutboundResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        headers,
        body: ResponseBody::Fixed(body),
    })
}

#[cfg(test)]
async fn recv_response_body(rx: &mut mpsc::Receiver<AsgiEvent>) -> Result<Bytes, AppError> {
    let (first_chunk, more) = recv_body_chunk(rx).await?;
    if !more {
        return Ok(first_chunk);
    }
    accumulate_remaining_chunks(rx, first_chunk).await
}

#[cfg(test)]
async fn recv_body_chunk(rx: &mut mpsc::Receiver<AsgiEvent>) -> Result<(Bytes, bool), AppError> {
    match rx.recv().await {
        Some(AsgiEvent::ResponseBody { body, more_body }) => Ok((body, more_body)),
        Some(AsgiEvent::ResponseStart { .. }) => Err(AppError::Internal(
            "ASGI protocol error: duplicate response start".to_owned(),
        )),
        Some(_) => Err(AppError::Internal(
            "ASGI protocol error: unexpected event during body collection".to_owned(),
        )),
        None => Ok((Bytes::new(), false)),
    }
}

#[cfg(test)]
async fn accumulate_remaining_chunks(
    rx: &mut mpsc::Receiver<AsgiEvent>,
    first: Bytes,
) -> Result<Bytes, AppError> {
    let mut buf = Vec::from(first.as_ref());
    loop {
        let (chunk, more) = recv_body_chunk(rx).await?;
        buf.extend_from_slice(&chunk);
        if !more {
            return Ok(Bytes::from(buf));
        }
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
    use http::header::HeaderMap;

    /// Build a `HeaderMap` from byte pair slices (test convenience).
    fn headers(pairs: &[(&[u8], &[u8])]) -> HeaderMap {
        let mut map = HeaderMap::with_capacity(pairs.len());
        for (name, value) in pairs {
            map.insert(
                http::HeaderName::from_bytes(name).unwrap(),
                http::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        map
    }

    #[tokio::test]
    async fn collect_asgi_response_simple() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: headers(&[(b"content-type", b"text/plain")]),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("hello"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let resp = collect_asgi_response(&mut rx).await.unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.headers.get("content-type").unwrap(), "text/plain");
        match &resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"hello"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn collect_asgi_response_chunked() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("hel"),
            more_body: true,
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("lo"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let resp = collect_asgi_response(&mut rx).await.unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        match &resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"hello"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn collect_asgi_response_missing_start() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("oops"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let result = collect_asgi_response(&mut rx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn collect_asgi_response_empty_body() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 204,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::new(),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let resp = collect_asgi_response(&mut rx).await.unwrap();
        assert_eq!(resp.status, StatusCode::NO_CONTENT);
        match &resp.body {
            ResponseBody::Fixed(b) => assert!(b.is_empty()),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn collect_asgi_response_channel_closed_before_start() {
        let (tx, mut rx) = mpsc::channel::<AsgiEvent>(4);
        drop(tx);

        let result = collect_asgi_response(&mut rx).await;
        assert!(result.is_err());
    }

    #[test]
    fn asgi_bridge_dispatch_debug() {
        let d = AsgiBridgeDispatch;
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[tokio::test]
    async fn collect_asgi_response_buffered_unchanged() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: headers(&[(b"content-type", b"application/json")]),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from(r#"{"ok":true}"#),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let resp = collect_asgi_response(&mut rx).await.unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );
        match &resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), br#"{"ok":true}"#),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn recv_response_start_pub_super_accessible() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 201,
            headers: headers(&[(b"x-test", b"yes")]),
        })
        .await
        .unwrap();

        let (status, headers) = recv_response_start(&mut rx).await.unwrap();
        assert_eq!(status, 201);
        assert_eq!(headers.len(), 1);
    }

    // ── recv_response_body error branches ────────────────────────────────

    #[tokio::test]
    async fn recv_response_body_duplicate_start() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        // Send another start where a body chunk is expected
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        drop(tx);

        let _ = recv_response_start(&mut rx).await.unwrap();
        let result = recv_response_body(&mut rx).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), AppError::Internal(msg) if msg.contains("duplicate"))
        );
    }

    #[tokio::test]
    async fn recv_response_body_unexpected_event() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        // Send a WS event during body collection
        tx.send(AsgiEvent::WsClose { code: 1000 }).await.unwrap();
        drop(tx);

        let _ = recv_response_start(&mut rx).await.unwrap();
        let result = recv_response_body(&mut rx).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), AppError::Internal(msg) if msg.contains("unexpected"))
        );
    }

    #[tokio::test]
    async fn recv_response_body_channel_close_partial() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: HeaderMap::new(),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("partial"),
            more_body: true,
        })
        .await
        .unwrap();
        drop(tx);

        let _ = recv_response_start(&mut rx).await.unwrap();
        // Channel closes mid-stream — returns whatever was collected
        let result = recv_response_body(&mut rx).await.unwrap();
        assert_eq!(result.as_ref(), b"partial");
    }

    #[tokio::test]
    async fn recv_response_start_unexpected_ws_event() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::WsSend {
            text: Some("hello".to_owned()),
            bytes: None,
        })
        .await
        .unwrap();
        drop(tx);

        let result = recv_response_start(&mut rx).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), AppError::Internal(msg) if msg.contains("unexpected"))
        );
    }
}
