//! Streaming ASGI response body.
//!
//! [`AsgiBodyStream`] wraps an mpsc channel of body chunks into a
//! [`futures_core::Stream`] suitable for HTTP chunked/SSE responses.

use super::scope::AsgiEvent;
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
    pub(super) fn new(
        rx: mpsc::Receiver<AsgiEvent>,
        initial_chunk: Option<Bytes>,
        disconnect_tx: Option<oneshot::Sender<()>>,
    ) -> Self {
        Self {
            rx,
            initial_chunk,
            disconnect_tx,
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
            tracing::trace!(name: "apx.asgi.streaming.initial_chunk", chunk_len = chunk.len(), "body_stream: initial chunk");
            return Poll::Ready(Some(Ok(chunk)));
        }

        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(AsgiEvent::ResponseBody { body, more_body })) => {
                tracing::trace!(
                    name: "apx.asgi.streaming.chunk_received",
                    body_len = body.len(),
                    more_body,
                    "body_stream: chunk received"
                );
                if !more_body {
                    self.done = true;
                }
                Poll::Ready(Some(Ok(body)))
            }
            Poll::Ready(Some(_) | None) => {
                tracing::trace!(name: "apx.asgi.streaming.channel_closed_or_unexpected", "body_stream: channel closed or unexpected event");
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                tracing::trace!(name: "apx.asgi.streaming.pending", "body_stream: pending (waiting for next chunk)");
                Poll::Pending
            }
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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
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
        let mut stream = AsgiBodyStream::new(rx, None, Some(disconnect_tx));
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
        let mut stream = AsgiBodyStream::new(rx, None, Some(disconnect_tx));
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
        let mut stream = AsgiBodyStream::new(rx, None, Some(disconnect_tx));
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
        let mut stream = AsgiBodyStream::new(rx, Some(Bytes::from("hello ")), Some(disconnect_tx));
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
        let stream = AsgiBodyStream::new(rx, None, Some(disconnect_tx));
        drop(stream);

        // disconnect_rx should have received the signal.
        assert!(disconnect_rx.await.is_ok());
    }
}
