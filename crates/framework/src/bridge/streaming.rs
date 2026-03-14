//! Streaming ASGI response builder.
//!
//! Converts an ASGI send channel into an [`OutboundResponse`] with a
//! [`ResponseBody::Stream`] body. Used for SSE and `StreamingResponse` routes.

use super::asgi::AsgiEvent;
use super::asgi_dispatch::recv_response_start;
use crate::error::AppError;
use crate::transport::types::{OutboundResponse, ResponseBody};
use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::{mpsc, oneshot};

/// Stream wrapper over an ASGI send channel.
///
/// Yields `ResponseBody` chunks until `more_body=false` or the channel closes.
/// Fires `disconnect_tx` on drop to signal the ASGI handler via `http.disconnect`.
pub struct AsgiBodyStream {
    rx: mpsc::Receiver<AsgiEvent>,
    initial_chunk: Option<Bytes>,
    disconnect_tx: Option<oneshot::Sender<()>>,
    done: bool,
}

impl AsgiBodyStream {
    /// Create a new body stream with an optional initial chunk and disconnect signal.
    fn new(
        rx: mpsc::Receiver<AsgiEvent>,
        initial_chunk: Option<Bytes>,
        disconnect_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            rx,
            initial_chunk,
            disconnect_tx: Some(disconnect_tx),
            done: false,
        }
    }
}

impl futures_core::Stream for AsgiBodyStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }

        if let Some(chunk) = self.initial_chunk.take() {
            return Poll::Ready(Some(Ok(chunk)));
        }

        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(AsgiEvent::ResponseBody { body, more_body })) => {
                if !more_body {
                    self.done = true;
                }
                Poll::Ready(Some(Ok(body)))
            }
            Poll::Ready(Some(_) | None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for AsgiBodyStream {
    fn drop(&mut self) {
        // Signal disconnect to AsgiReceive. Sending () is enough —
        // the receiver resolves its Future with http.disconnect.
        // If the coroutine already finished, the signal is harmless.
        if let Some(tx) = self.disconnect_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Classify an ASGI response from a send channel as fixed or streaming.
///
/// Reads `ResponseStart` for status/headers, then the first body chunk.
/// `more_body=false` → `Fixed`. `more_body=true` → `Stream`.
pub(super) async fn classify_response(
    mut rx: mpsc::Receiver<AsgiEvent>,
    disconnect_tx: oneshot::Sender<()>,
) -> Result<OutboundResponse, AppError> {
    let (status, headers) = recv_response_start(&mut rx).await?;
    let status =
        http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);

    match rx.recv().await {
        Some(AsgiEvent::ResponseBody {
            body,
            more_body: false,
        }) => {
            // Fixed — disconnect_tx drops here, fires disconnect signal.
            Ok(OutboundResponse {
                status,
                headers,
                body: ResponseBody::Fixed(body),
            })
        }
        Some(AsgiEvent::ResponseBody {
            body,
            more_body: true,
        }) => {
            // Streaming — pass disconnect_tx to stream for lifetime tracking.
            let stream = AsgiBodyStream::new(rx, Some(body), disconnect_tx);
            Ok(OutboundResponse {
                status,
                headers,
                body: ResponseBody::Stream(Box::pin(stream)),
            })
        }
        Some(_) => Err(AppError::Internal(
            "ASGI protocol error: unexpected event after response start".to_owned(),
        )),
        None => Ok(OutboundResponse {
            status,
            headers,
            body: ResponseBody::Fixed(Bytes::new()),
        }),
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
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn asgi_body_stream_single_chunk() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("hello"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut stream = AsgiBodyStream::new(rx, None, disconnect_tx);
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.as_ref(), b"hello");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn asgi_body_stream_multiple_chunks() {
        let (tx, rx) = mpsc::channel(4);
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

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut stream = AsgiBodyStream::new(rx, None, disconnect_tx);
        let c1 = stream.next().await.unwrap().unwrap();
        assert_eq!(c1.as_ref(), b"hel");
        let c2 = stream.next().await.unwrap().unwrap();
        assert_eq!(c2.as_ref(), b"lo");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn asgi_body_stream_channel_closed() {
        let (tx, rx) = mpsc::channel::<AsgiEvent>(4);
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut stream = AsgiBodyStream::new(rx, None, disconnect_tx);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn asgi_body_stream_initial_chunk() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("world"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut stream = AsgiBodyStream::new(rx, Some(Bytes::from("hello ")), disconnect_tx);
        let c1 = stream.next().await.unwrap().unwrap();
        assert_eq!(c1.as_ref(), b"hello ");
        let c2 = stream.next().await.unwrap().unwrap();
        assert_eq!(c2.as_ref(), b"world");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn asgi_body_stream_drop_fires_disconnect() {
        let (disconnect_tx, disconnect_rx) = oneshot::channel();
        let (_tx, rx) = mpsc::channel::<AsgiEvent>(4);
        let stream = AsgiBodyStream::new(rx, None, disconnect_tx);
        drop(stream);

        // disconnect_rx should have received the signal.
        assert!(disconnect_rx.await.is_ok());
    }

    /// Build a `HeaderMap` from byte pair slices (test convenience).
    fn headers(pairs: &[(&[u8], &[u8])]) -> http::header::HeaderMap {
        let mut map = http::header::HeaderMap::with_capacity(pairs.len());
        for (name, value) in pairs {
            map.insert(
                http::HeaderName::from_bytes(name).unwrap(),
                http::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        map
    }

    #[tokio::test]
    async fn classify_fixed_single_body() {
        let (tx, rx) = mpsc::channel(4);
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

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let resp = classify_response(rx, disconnect_tx).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
        assert_eq!(resp.headers.get("content-type").unwrap(), "text/plain");
        match resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"hello"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn classify_streaming_body() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: headers(&[(b"content-type", b"text/event-stream")]),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("data: hello\n\n"),
            more_body: true,
        })
        .await
        .unwrap();
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let resp = classify_response(rx, disconnect_tx).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "text/event-stream"
        );
        match resp.body {
            ResponseBody::Stream(mut stream) => {
                // The initial chunk should be the first body.
                use futures_core::Stream;
                let waker = futures_util::task::noop_waker();
                let mut cx = Context::from_waker(&waker);
                match Pin::new(&mut stream).poll_next(&mut cx) {
                    Poll::Ready(Some(Ok(chunk))) => {
                        assert_eq!(chunk.as_ref(), b"data: hello\n\n");
                    }
                    other => panic!("expected Ready(Some(Ok(...))), got {other:?}"),
                }
            }
            ResponseBody::Fixed(_) => panic!("expected Stream body"),
        }
    }

    #[tokio::test]
    async fn classify_empty_body_channel_closed() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 204,
            headers: headers(&[]),
        })
        .await
        .unwrap();
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let resp = classify_response(rx, disconnect_tx).await.unwrap();
        assert_eq!(resp.status, http::StatusCode::NO_CONTENT);
        match resp.body {
            ResponseBody::Fixed(b) => assert!(b.is_empty()),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn classify_missing_response_start() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("oops"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let result = classify_response(rx, disconnect_tx).await;
        assert!(result.is_err());
    }
}
