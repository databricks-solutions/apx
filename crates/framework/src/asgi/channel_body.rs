//! Streaming response body backed by a tokio mpsc channel.
//!
//! [`ChannelBody`] wraps `mpsc::UnboundedReceiver<Bytes>` and implements
//! `futures_core::Stream`. It replaces `AsgiBodyStream` for the 3-thread
//! architecture — `SlotSend` pushes chunks from Thread 2, hyper consumes
//! them on Thread 1 via `ResponseBody::Stream`.

use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// Streaming response body fed by an mpsc channel.
///
/// EOF is signaled by dropping the sender half.
pub struct ChannelBody {
    rx: mpsc::UnboundedReceiver<Bytes>,
}

impl ChannelBody {
    /// Wrap a receiver into a stream of body chunks.
    pub fn new(rx: mpsc::UnboundedReceiver<Bytes>) -> Self {
        Self { rx }
    }
}

crate::opaque_debug!(ChannelBody);

impl futures_core::Stream for ChannelBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn channel_body_single_chunk() {
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Bytes::from("hello")).unwrap();
        drop(tx);

        let mut body = ChannelBody::new(rx);
        let chunk = body.next().await.unwrap().unwrap();
        assert_eq!(chunk, Bytes::from("hello"));
        assert!(body.next().await.is_none());
    }

    #[tokio::test]
    async fn channel_body_multiple_chunks() {
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Bytes::from("hel")).unwrap();
        tx.send(Bytes::from("lo")).unwrap();
        drop(tx);

        let mut body = ChannelBody::new(rx);
        assert_eq!(body.next().await.unwrap().unwrap(), Bytes::from("hel"));
        assert_eq!(body.next().await.unwrap().unwrap(), Bytes::from("lo"));
        assert!(body.next().await.is_none());
    }

    #[tokio::test]
    async fn channel_body_empty() {
        let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
        drop(tx);
        let mut body = ChannelBody::new(rx);
        assert!(body.next().await.is_none());
    }
}
