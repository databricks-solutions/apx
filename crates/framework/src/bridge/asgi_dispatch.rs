//! ASGI bridge dispatch — runs handlers through scope/receive/send.
//!
//! This dispatch path is used for routes with `Depends()`, streaming
//! responses, or `Request`/`Response` parameter injection. It constructs
//! Rust-backed ASGI objects from [`InboundRequest`] and collects the
//! response from the ASGI send channel.

use crate::bridge::asgi::{AsgiEvent, AsgiReceive, AsgiSend, build_http_scope};
use crate::bridge::dispatch::{AppState, HandlerDispatch};
use crate::error::{AppError, BodyParseKind};
use crate::route::{BoundRoute, HandlerKind, ResponseType};
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::StatusCode;
use http::header::HeaderMap;
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

            // 2. Build ASGI objects and get handler coroutine (brief GIL hold)
            let (send_tx, mut send_rx) = mpsc::channel::<AsgiEvent>(ASGI_CHANNEL_SIZE);

            let coro = Python::attach(|py| -> Result<Py<PyAny>, AppError> {
                let asgi_callable = route
                    .fastapi_app
                    .as_ref()
                    .map_or_else(|| route.handler.inner(), |a| a.inner());

                let scope =
                    build_http_scope(py, &request, route.fastapi_app.as_ref().map(|a| a.inner()))
                        .map_err(|e| AppError::Internal(format!("build scope: {e}")))?;

                let receive = if body_bytes.is_empty() {
                    AsgiReceive::empty()
                } else {
                    AsgiReceive::http(body_bytes)
                };
                let send = AsgiSend::new(send_tx);

                let receive_obj = Py::new(py, receive)
                    .map_err(|e| AppError::Internal(format!("wrap receive: {e}")))?;
                let send_obj =
                    Py::new(py, send).map_err(|e| AppError::Internal(format!("wrap send: {e}")))?;

                asgi_callable
                    .call(py, (scope, receive_obj, send_obj), None)
                    .map_err(|e| AppError::Internal(format!("handler call: {e}")))
            })?;

            // 3. Drive handler coroutine on the persistent event loop.
            //    This runs concurrently with Tokio response collection below.
            //    BackgroundTasks, contextvars, and get_running_loop() all work
            //    correctly because the coroutine runs on a real asyncio loop.
            let loop_handle = app_state.loop_handle.clone();
            let handler_task = tokio::spawn(async move { loop_handle.drive_coroutine(coro).await });

            // 4. Branch: streaming vs buffered response collection
            let is_streaming = matches!(route.manifest.kind, HandlerKind::SSE)
                || matches!(
                    route.manifest.response_type,
                    ResponseType::StreamingResponse
                );

            if is_streaming {
                super::streaming::stream_asgi_response(send_rx, handler_task).await
            } else {
                let response = collect_asgi_response(&mut send_rx).await?;
                let _ = handler_task.await;
                Ok(response)
            }
        })
    }
}

/// Collect ASGI send events into an [`OutboundResponse`].
///
/// Expects `ResponseStart` followed by one or more `ResponseBody` chunks.
pub async fn collect_asgi_response(
    rx: &mut mpsc::Receiver<AsgiEvent>,
) -> Result<OutboundResponse, AppError> {
    // First event must be ResponseStart
    let (status, raw_headers) = recv_response_start(rx).await?;

    // Collect body chunks
    let body = recv_response_body(rx).await?;

    // Build header map from raw byte pairs
    let headers = build_header_map(&raw_headers)?;

    Ok(OutboundResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        headers,
        body: ResponseBody::Fixed(body),
    })
}

/// Receive the `ResponseStart` event from the channel.
pub(super) async fn recv_response_start(
    rx: &mut mpsc::Receiver<AsgiEvent>,
) -> Result<(u16, Vec<(Vec<u8>, Vec<u8>)>), AppError> {
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

/// Receive and concatenate `ResponseBody` chunks from the channel.
async fn recv_response_body(rx: &mut mpsc::Receiver<AsgiEvent>) -> Result<Bytes, AppError> {
    let mut buf = Vec::new();
    loop {
        match rx.recv().await {
            Some(AsgiEvent::ResponseBody { body, more_body }) => {
                buf.extend_from_slice(&body);
                if !more_body {
                    return Ok(Bytes::from(buf));
                }
            }
            Some(AsgiEvent::ResponseStart { .. }) => {
                return Err(AppError::Internal(
                    "ASGI protocol error: duplicate response start".to_owned(),
                ));
            }
            Some(_) => {
                return Err(AppError::Internal(
                    "ASGI protocol error: unexpected event during body collection".to_owned(),
                ));
            }
            None => return Ok(Bytes::from(buf)),
        }
    }
}

/// Convert raw ASGI header byte pairs to an [`http::HeaderMap`].
pub(super) fn build_header_map(raw: &[(Vec<u8>, Vec<u8>)]) -> Result<HeaderMap, AppError> {
    let mut headers = HeaderMap::with_capacity(raw.len());
    for (name, value) in raw {
        let name = http::HeaderName::from_bytes(name)
            .map_err(|e| AppError::Internal(format!("invalid header name: {e}")))?;
        let value = http::HeaderValue::from_bytes(value)
            .map_err(|e| AppError::Internal(format!("invalid header value: {e}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
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

    #[tokio::test]
    async fn collect_asgi_response_simple() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"text/plain".to_vec())],
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
            headers: Vec::new(),
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
            headers: Vec::new(),
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

    #[test]
    fn build_header_map_valid() {
        let raw = vec![
            (b"content-type".to_vec(), b"application/json".to_vec()),
            (b"x-custom".to_vec(), b"value".to_vec()),
        ];
        let headers = build_header_map(&raw).unwrap();
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert_eq!(headers.get("x-custom").unwrap(), "value");
    }

    #[test]
    fn build_header_map_invalid_name() {
        let raw = vec![(b"invalid header\x00".to_vec(), b"value".to_vec())];
        assert!(build_header_map(&raw).is_err());
    }

    #[tokio::test]
    async fn collect_asgi_response_buffered_unchanged() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"application/json".to_vec())],
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
            headers: vec![(b"x-test".to_vec(), b"yes".to_vec())],
        })
        .await
        .unwrap();

        let (status, headers) = recv_response_start(&mut rx).await.unwrap();
        assert_eq!(status, 201);
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn build_header_map_pub_super_accessible() {
        let raw = vec![(b"x-test".to_vec(), b"value".to_vec())];
        let headers = build_header_map(&raw).unwrap();
        assert_eq!(headers.get("x-test").unwrap(), "value");
    }

    // ── recv_response_body error branches ────────────────────────────────

    #[tokio::test]
    async fn recv_response_body_duplicate_start() {
        let (tx, mut rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: Vec::new(),
        })
        .await
        .unwrap();
        // Send another start where a body chunk is expected
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: Vec::new(),
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
            headers: Vec::new(),
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
            headers: Vec::new(),
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
