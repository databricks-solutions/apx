//! Request dispatch abstraction.
//!
//! Defines the [`Dispatch`] trait — the extension seam between the HTTP
//! service layer and the application layer. The service layer calls
//! `dispatch()` after health probes, concurrency checks, and timeout
//! wrapping. For WebSocket upgrades, the service calls `dispatch_ws()`
//! with the raw hyper request (before body consumption).

use crate::transport::types::ResponseBody;
use crate::transport::{InboundRequest, OutboundResponse};
use bytes::Bytes;
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

/// Dispatch a request to the application layer.
///
/// Implementations decide the strategy: ASGI bridge, direct dispatch, etc.
/// The service layer calls `dispatch()` after health probes, concurrency
/// checks, and timeout wrapping.
pub trait Dispatch: Send + Sync + std::fmt::Debug {
    /// Handle a single inbound request.
    fn dispatch(
        &self,
        request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = OutboundResponse> + Send>>;

    /// Handle a WebSocket upgrade request.
    ///
    /// Called with the raw hyper request before body consumption, since
    /// `hyper_tungstenite::upgrade` consumes the request. The default
    /// implementation returns 400 Bad Request.
    fn dispatch_ws(
        &self,
        _request: Request<Incoming>,
        _server_addr: SocketAddr,
        _client_addr: Option<SocketAddr>,
    ) -> Pin<Box<dyn Future<Output = Response<ResponseBody>> + Send>> {
        Box::pin(async {
            // 400 Bad Request — WebSocket not supported by this dispatch.
            Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .header(http::header::CONTENT_TYPE, "text/plain")
                .body(ResponseBody::Fixed(Bytes::from_static(
                    b"websocket not supported",
                )))
                .unwrap_or_else(|_| unreachable!())
        })
    }
}
