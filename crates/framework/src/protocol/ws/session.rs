//! WebSocket bridge — upgrades HTTP connections and bridges frames between
//! tungstenite and ASGI receive/send channels.
//!
//! The flow:
//! 1. [`is_websocket_upgrade`] detects upgrade requests at the service layer.
//! 2. [`handle_upgrade`] performs the hyper-tungstenite handshake and spawns
//!    a background session task.
//! 3. The session bridges tungstenite frames ↔ ASGI `websocket.*` events
//!    through mpsc channels, with the ASGI app driven by the Rust scheduler.

use crate::asgi::scope::{
    AsgiEvent, AsgiSend, AsgiWsReceive, ScopeInterns, WsIncomingEvent, build_ws_scope,
};
use crate::protocol::http::error::AppError;
use crate::supervision::worker_context::WorkerContext;
use crate::transport::types::{
    BodyStream, InboundRequest, ProtocolVersion, ResponseBody, TransportKind,
};
use bytes::Bytes;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_tungstenite::tungstenite;
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tungstenite::Message;
use tungstenite::protocol::frame::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;

/// Buffer size for both incoming and outgoing WebSocket channels.
const WS_CHANNEL_CAPACITY: usize = 32;

/// Default WebSocket close code (normal closure).
const WS_CLOSE_NORMAL: u16 = 1000;

// ── Upgrade detection ───────────────────────────────────────────────────

/// Check if a request is a WebSocket upgrade request.
pub fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    hyper_tungstenite::is_upgrade_request(req)
}

// ── Upgrade handler ─────────────────────────────────────────────────────

/// Perform the WebSocket upgrade handshake and spawn the session task.
///
/// Returns the 101 Switching Protocols response immediately. The actual
/// WebSocket session runs in a spawned tokio task.
///
/// # Errors
///
/// Returns an error if the upgrade handshake fails.
pub fn handle_upgrade(
    mut request: Request<Incoming>,
    server_addr: SocketAddr,
    client_addr: Option<SocketAddr>,
    app: Arc<Py<PyAny>>,
    interns: Arc<ScopeInterns>,
    ctx: Arc<WorkerContext>,
) -> Result<Response<ResponseBody>, AppError> {
    // Extract request metadata before the upgrade consumes the request.
    let inbound = extract_request_info(&request, server_addr, client_addr);

    // Perform the upgrade handshake.
    let (response, ws_future) = hyper_tungstenite::upgrade(&mut request, None)
        .map_err(|e| AppError::Internal(format!("websocket upgrade failed: {e}")))?;

    // Convert the Full<Bytes> body to our ResponseBody (101 body is empty).
    let (parts, _full_body) = response.into_parts();
    let response = Response::from_parts(parts, ResponseBody::Fixed(Bytes::new()));

    // Spawn the WebSocket session as a background task.
    tokio::spawn(ws_session(ws_future, inbound, app, interns, ctx));

    Ok(response)
}

// ── Request extraction ──────────────────────────────────────────────────

/// Extract request metadata from a borrowed hyper request.
///
/// Builds an [`InboundRequest`] with [`BodyStream::Empty`] since the WS
/// upgrade path doesn't use the HTTP body. Generic over the body type
/// because only request parts (URI, headers, method, version) are accessed.
fn extract_request_info<B>(
    req: &Request<B>,
    server_addr: SocketAddr,
    client_addr: Option<SocketAddr>,
) -> InboundRequest {
    let path = req.uri().path().to_owned();
    let query_string = req
        .uri()
        .query()
        .map(|q| Bytes::copy_from_slice(q.as_bytes()))
        .unwrap_or_default();
    let mut headers = req.headers().clone();
    crate::protocol::http::service::ensure_request_id(&mut headers);
    let method = req.method().clone();

    let protocol = match req.version() {
        hyper::Version::HTTP_10 => ProtocolVersion::Http10,
        hyper::Version::HTTP_2 => ProtocolVersion::H2,
        _ => ProtocolVersion::Http11,
    };

    InboundRequest::new(
        method,
        path,
        query_string,
        headers,
        BodyStream::Empty,
        protocol,
        TransportKind::Tcp,
        client_addr,
        server_addr,
        Vec::new(),
        http::Extensions::default(),
    )
}

// ── WebSocket session ───────────────────────────────────────────────────

/// Run a WebSocket session: bridge tungstenite frames ↔ ASGI events.
///
/// This is a long-lived tokio task that runs for the lifetime of the
/// WebSocket connection.
async fn ws_session(
    ws_future: hyper_tungstenite::HyperWebsocket,
    request: InboundRequest,
    app: Arc<Py<PyAny>>,
    interns: Arc<ScopeInterns>,
    ctx: Arc<WorkerContext>,
) {
    // Await the upgrade completion to get the WebSocket stream.
    let ws_stream = match ws_future.await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::error!(name: "apx.ws.upgrade_completion_failed", error = %e, "websocket upgrade completion failed");
            return;
        }
    };

    let (sink, stream) = ws_stream.split();

    // Create channels for ASGI ↔ tungstenite communication.
    let (incoming_tx, incoming_rx) = mpsc::channel::<WsIncomingEvent>(WS_CHANNEL_CAPACITY);
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<AsgiEvent>(WS_CHANNEL_CAPACITY);

    // Send initial connect event per ASGI spec.
    if incoming_tx.send(WsIncomingEvent::Connect).await.is_err() {
        return;
    }

    // Spawn forwarding tasks.
    let recv_handle = tokio::spawn(forward_incoming(stream, incoming_tx));
    let send_handle = tokio::spawn(forward_outgoing(outgoing_rx, sink));

    // Build scope, call app, submit to asyncio.
    let schedule_result = Python::attach(|py| -> Result<(), AppError> {
        let scope = build_ws_scope(py, &request, &interns)
            .map_err(|e| AppError::Internal(format!("ws scope build: {e}")))?;
        let receive = Py::new(py, AsgiWsReceive::new(incoming_rx))
            .map_err(|e| AppError::Internal(format!("wrap ws receive: {e}")))?;
        let send = Py::new(py, AsgiSend::new(outgoing_tx))
            .map_err(|e| AppError::Internal(format!("wrap ws send: {e}")))?;
        ctx.call_soon_threadsafe
            .call1(py, (&ctx.launch_fn, &*app, &scope, &receive, &send))
            .map_err(|e| AppError::Internal(format!("submit ws to asyncio: {e}")))?;
        Ok(())
    });
    if let Err(e) = schedule_result {
        tracing::error!(name: "apx.ws.schedule_coroutine_failed", error = %e, "failed to schedule websocket coroutine");
    }

    // Clean up forwarding tasks.
    recv_handle.abort();
    send_handle.abort();
}

// ── Frame forwarding ────────────────────────────────────────────────────

/// Forward incoming tungstenite frames to the ASGI receive channel.
///
/// Generic over the stream type so it can be tested with mock streams.
async fn forward_incoming<S>(mut stream: S, tx: mpsc::Sender<WsIncomingEvent>)
where
    S: Stream<Item = Result<Message, tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await {
            Some(Ok(msg)) => {
                let event = match msg {
                    Message::Text(t) => WsIncomingEvent::Receive {
                        text: Some(t.to_string()),
                        bytes: None,
                    },
                    Message::Binary(b) => WsIncomingEvent::Receive {
                        text: None,
                        bytes: Some(b),
                    },
                    Message::Close(frame) => {
                        let code = frame
                            .as_ref()
                            .map_or(WS_CLOSE_NORMAL, |f| u16::from(f.code));
                        let event = WsIncomingEvent::Disconnect { code };
                        let _ = tx.send(event).await;
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
            Some(Err(e)) => {
                tracing::debug!(name: "apx.ws.read_error", error = %e, "websocket read error");
                let _ = tx
                    .send(WsIncomingEvent::Disconnect {
                        code: WS_CLOSE_NORMAL,
                    })
                    .await;
                break;
            }
            None => {
                // Stream ended — send disconnect.
                let _ = tx
                    .send(WsIncomingEvent::Disconnect {
                        code: WS_CLOSE_NORMAL,
                    })
                    .await;
                break;
            }
        }
    }
}

/// Forward outgoing ASGI events to the tungstenite sink.
///
/// Generic over the sink type so it can be tested with mock sinks.
async fn forward_outgoing<K>(mut rx: mpsc::Receiver<AsgiEvent>, mut sink: K)
where
    K: Sink<Message, Error = tungstenite::Error> + Unpin,
{
    let mut accepted = false;

    while let Some(event) = rx.recv().await {
        match event {
            AsgiEvent::WsAccept { .. } => {
                // Protocol acknowledgement — 101 already sent, no WS frame needed.
                accepted = true;
            }
            AsgiEvent::WsSend { text, bytes } => {
                if !accepted {
                    tracing::warn!(name: "apx.ws.send_before_accept", "websocket send before accept — dropping frame");
                    continue;
                }
                let msg = if let Some(t) = text {
                    Message::text(t)
                } else if let Some(b) = bytes {
                    Message::binary(b)
                } else {
                    continue;
                };
                if let Err(e) = sink.send(msg).await {
                    tracing::debug!(name: "apx.ws.write_error", error = %e, "websocket write error");
                    break;
                }
            }
            AsgiEvent::WsClose { code } => {
                let close_frame = CloseFrame {
                    code: CloseCode::from(code),
                    reason: "".into(),
                };
                let _ = sink.send(Message::Close(Some(close_frame))).await;
                break;
            }
            AsgiEvent::ResponseStart { .. } | AsgiEvent::ResponseBody { .. } => {
                tracing::error!(name: "apx.ws.http_response_in_ws_context", "HTTP response event in websocket context — ignoring");
            }
        }
    }

    // Best-effort close the sink when the channel closes.
    let _ = sink.close().await;
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use std::pin::Pin;

    /// Empty body type usable in test requests (hyper's `Incoming` has no `Default`).
    type EmptyBody = http_body_util::Empty<Bytes>;

    fn empty_body() -> EmptyBody {
        http_body_util::Empty::new()
    }

    // ── is_websocket_upgrade ────────────────────────────────────────────

    #[test]
    fn is_websocket_upgrade_positive() {
        let req = Request::builder()
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .body(empty_body())
            .unwrap();
        assert!(hyper_tungstenite::is_upgrade_request(&req));
    }

    #[test]
    fn is_websocket_upgrade_negative() {
        let req = Request::builder().method("GET").body(empty_body()).unwrap();
        assert!(!hyper_tungstenite::is_upgrade_request(&req));
    }

    #[test]
    fn is_websocket_upgrade_case_insensitive() {
        let req = Request::builder()
            .header("Connection", "upgrade")
            .header("Upgrade", "WEBSOCKET")
            .body(empty_body())
            .unwrap();
        assert!(hyper_tungstenite::is_upgrade_request(&req));
    }

    // ── extract_request_info ────────────────────────────────────────────

    #[test]
    fn extract_request_info_preserves_fields() {
        let req = Request::builder()
            .uri("/ws/chat?token=abc")
            .header("Host", "localhost")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .body(empty_body())
            .unwrap();

        let server = SocketAddr::from(([127, 0, 0, 1], 8080));
        let client = SocketAddr::from(([10, 0, 0, 1], 54321));
        let inbound = extract_request_info(&req, server, Some(client));

        assert_eq!(inbound.path, "/ws/chat");
        assert_eq!(inbound.query_string.as_ref(), b"token=abc");
        assert_eq!(inbound.server_addr, server);
        assert_eq!(inbound.client_addr, Some(client));
        assert!(inbound.headers.contains_key("host"));
        assert!(!inbound.has_body());
    }

    #[test]
    fn extract_request_info_no_query() {
        let req = Request::builder().uri("/ws").body(empty_body()).unwrap();
        let server = SocketAddr::from(([127, 0, 0, 1], 8080));
        let inbound = extract_request_info(&req, server, None);
        assert_eq!(inbound.path, "/ws");
        assert!(inbound.query_string.is_empty());
        assert!(inbound.client_addr.is_none());
    }

    #[test]
    fn extract_request_info_headers() {
        let req = Request::builder()
            .uri("/ws")
            .header("X-Custom", "test-value")
            .header("Authorization", "Bearer xyz")
            .body(empty_body())
            .unwrap();
        let server = SocketAddr::from(([0, 0, 0, 0], 3000));
        let inbound = extract_request_info(&req, server, None);
        assert_eq!(inbound.headers.get("x-custom").unwrap(), "test-value");
        assert_eq!(inbound.headers.get("authorization").unwrap(), "Bearer xyz");
    }

    // ── forward_incoming ────────────────────────────────────────────────

    #[tokio::test]
    async fn forward_incoming_text_message() {
        let stream = futures_util::stream::iter(vec![Ok(Message::text("hello"))]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(forward_incoming(stream, tx));

        match rx.recv().await.unwrap() {
            WsIncomingEvent::Receive { text, bytes } => {
                assert_eq!(text.as_deref(), Some("hello"));
                assert!(bytes.is_none());
            }
            other => panic!("expected Receive, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forward_incoming_binary_message() {
        let stream = futures_util::stream::iter(vec![Ok(Message::binary(vec![1u8, 2, 3]))]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(forward_incoming(stream, tx));

        match rx.recv().await.unwrap() {
            WsIncomingEvent::Receive { text, bytes } => {
                assert!(text.is_none());
                assert_eq!(bytes.as_deref(), Some(&[1u8, 2, 3][..]));
            }
            other => panic!("expected Receive, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forward_incoming_close_frame() {
        let close = Message::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "bye".into(),
        }));
        let stream = futures_util::stream::iter(vec![Ok(close)]);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(forward_incoming(stream, tx));

        match rx.recv().await.unwrap() {
            WsIncomingEvent::Disconnect { code } => assert_eq!(code, 1000),
            other => panic!("expected Disconnect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn forward_incoming_stream_end() {
        let stream = futures_util::stream::empty::<Result<Message, tungstenite::Error>>();
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(forward_incoming(stream, tx));

        match rx.recv().await.unwrap() {
            WsIncomingEvent::Disconnect { code } => assert_eq!(code, WS_CLOSE_NORMAL),
            other => panic!("expected Disconnect, got {other:?}"),
        }
    }

    // ── forward_outgoing ────────────────────────────────────────────────

    /// Sink that collects messages + shared Vec for assertions.
    type MockSink = Pin<Box<dyn Sink<Message, Error = tungstenite::Error> + Send>>;

    fn mock_sink() -> (MockSink, Arc<std::sync::Mutex<Vec<Message>>>) {
        let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
        let msgs = Arc::clone(&messages);
        let sink = futures_util::sink::unfold(msgs, |msgs, msg: Message| async move {
            msgs.lock().unwrap().push(msg);
            Ok::<_, tungstenite::Error>(msgs)
        });
        (Box::pin(sink), messages)
    }

    #[tokio::test]
    async fn forward_outgoing_accept_then_send() {
        let (event_tx, event_rx) = mpsc::channel(8);
        let (sink, messages) = mock_sink();

        event_tx
            .send(AsgiEvent::WsAccept {
                subprotocol: None,
                headers: Vec::new(),
            })
            .await
            .unwrap();
        event_tx
            .send(AsgiEvent::WsSend {
                text: Some("world".to_owned()),
                bytes: None,
            })
            .await
            .unwrap();
        event_tx
            .send(AsgiEvent::WsClose { code: 1000 })
            .await
            .unwrap();
        drop(event_tx);

        forward_outgoing(event_rx, sink).await;

        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 2); // WsSend + WsClose
        assert_eq!(msgs[0], Message::text("world"));
        assert!(matches!(msgs[1], Message::Close(Some(_))));
    }

    #[tokio::test]
    async fn forward_outgoing_close() {
        let (event_tx, event_rx) = mpsc::channel(8);
        let (sink, messages) = mock_sink();

        event_tx
            .send(AsgiEvent::WsAccept {
                subprotocol: None,
                headers: Vec::new(),
            })
            .await
            .unwrap();
        event_tx
            .send(AsgiEvent::WsClose { code: 1001 })
            .await
            .unwrap();
        drop(event_tx);

        forward_outgoing(event_rx, sink).await;

        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            Message::Close(Some(frame)) => assert_eq!(u16::from(frame.code), 1001),
            other => panic!("expected Close, got {other:?}"),
        }
    }

    // Note: `dispatch_ws` default (400 response) and `is_websocket_upgrade`
    // are tested indirectly via the `service.rs` integration path and the
    // `is_upgrade_request` tests above. hyper's `Incoming` type has no public
    // constructors, so direct unit tests require a real HTTP connection.
}
