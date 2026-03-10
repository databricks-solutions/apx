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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Stream wrapper over an ASGI send channel.
///
/// Yields `ResponseBody` chunks until `more_body=false` or the channel closes.
/// Owns the handler `JoinHandle` — aborts it on drop (client disconnect cleanup).
pub struct AsgiBodyStream {
    rx: mpsc::Receiver<AsgiEvent>,
    handler_task: Option<JoinHandle<Result<(), AppError>>>,
    done: bool,
}

impl AsgiBodyStream {
    #[cfg(test)]
    fn new(rx: mpsc::Receiver<AsgiEvent>, handler_task: JoinHandle<Result<(), AppError>>) -> Self {
        Self {
            rx,
            handler_task: Some(handler_task),
            done: false,
        }
    }

    /// Wrap a handler task that returns a value, discarding the value.
    fn from_valued_task<T: Send + 'static>(
        rx: mpsc::Receiver<AsgiEvent>,
        task: JoinHandle<Result<T, AppError>>,
    ) -> Self {
        let wrapper = tokio::spawn(async move {
            task.await
                .map_err(|e| AppError::Internal(format!("handler task join: {e}")))?
                .map(|_| ())
        });
        Self {
            rx,
            handler_task: Some(wrapper),
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
        if let Some(task) = self.handler_task.take() {
            task.abort();
        }
    }
}

/// Build a streaming [`OutboundResponse`] from an ASGI send channel.
///
/// Reads `ResponseStart` for status and headers, then wraps remaining
/// body chunks in an [`AsgiBodyStream`].
pub async fn stream_asgi_response<T: Send + 'static>(
    mut rx: mpsc::Receiver<AsgiEvent>,
    handler_task: JoinHandle<Result<T, AppError>>,
) -> Result<OutboundResponse, AppError> {
    let (status, headers) = recv_response_start(&mut rx).await?;
    let status =
        http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);

    let stream = AsgiBodyStream::from_valued_task(rx, handler_task);

    Ok(OutboundResponse {
        status,
        headers,
        body: ResponseBody::Stream(Box::pin(stream)),
    })
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

    fn spawn_noop_handler() -> JoinHandle<Result<(), AppError>> {
        tokio::spawn(async { Ok(()) })
    }

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

        let mut stream = AsgiBodyStream::new(rx, spawn_noop_handler());
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

        let mut stream = AsgiBodyStream::new(rx, spawn_noop_handler());
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

        let mut stream = AsgiBodyStream::new(rx, spawn_noop_handler());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn asgi_body_stream_drop_aborts_task() {
        let task = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(())
        });
        let (_tx, rx) = mpsc::channel::<AsgiEvent>(4);

        let stream = AsgiBodyStream::new(rx, task);
        let abort_handle = stream.handler_task.as_ref().unwrap().abort_handle();
        drop(stream);

        // Yield to let the runtime process the abort
        tokio::task::yield_now().await;
        assert!(abort_handle.is_finished());
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
    async fn stream_asgi_response_success() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 200,
            headers: headers(&[(b"content-type", b"text/event-stream")]),
        })
        .await
        .unwrap();
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("data: hello\n\n"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let resp = stream_asgi_response(rx, spawn_noop_handler())
            .await
            .unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "text/event-stream"
        );
        match resp.body {
            ResponseBody::Stream(_) => {}
            ResponseBody::Fixed(_) => panic!("expected Stream body"),
        }
    }

    #[tokio::test]
    async fn stream_asgi_response_missing_start() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseBody {
            body: Bytes::from("oops"),
            more_body: false,
        })
        .await
        .unwrap();
        drop(tx);

        let result = stream_asgi_response(rx, spawn_noop_handler()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stream_asgi_response_status_and_headers() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(AsgiEvent::ResponseStart {
            status: 201,
            headers: headers(&[(b"x-custom", b"value"), (b"content-type", b"text/plain")]),
        })
        .await
        .unwrap();
        drop(tx);

        let resp = stream_asgi_response(rx, spawn_noop_handler())
            .await
            .unwrap();
        assert_eq!(resp.status, http::StatusCode::CREATED);
        assert_eq!(resp.headers.get("x-custom").unwrap(), "value");
        assert_eq!(resp.headers.get("content-type").unwrap(), "text/plain");
    }
}
