//! Handler dispatch trait and shared dispatch infrastructure.
//!
//! The sole concrete implementation is [`super::asgi_dispatch::AsgiBridgeDispatch`] —
//! all routes are dispatched through the FastAPI ASGI app.

use crate::error::AppError;
use crate::event_loop::EventLoopHandle;
use crate::route::BoundRoute;
use crate::transport::types::{InboundRequest, OutboundResponse};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Lifecycle-scoped state shared across all routes in a single worker.
#[derive(Clone)]
pub struct AppState {
    /// Max request body size in bytes.
    pub max_body_limit: crate::route::BodyLimit,
    /// Handle to the persistent asyncio event loop.
    pub loop_handle: EventLoopHandle,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("max_body_limit", &self.max_body_limit)
            .field("loop_handle", &self.loop_handle)
            .finish_non_exhaustive()
    }
}

/// Handles the full request lifecycle for a specific handler kind.
///
/// Implementations work entirely on transport-neutral types. The axum
/// boundary lives in `bridge/mod.rs::python_handler` only.
pub trait HandlerDispatch: Send + Sync + std::fmt::Debug {
    /// Process a request and return a transport-neutral response.
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>>;
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn app_state_debug() {
        let mut event_loop = crate::event_loop::EventLoop::start().unwrap();
        let state = AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
            loop_handle: event_loop.handle(),
        };
        let dbg = format!("{state:?}");
        assert!(dbg.contains("AppState"));
        assert!(dbg.contains("max_body_limit"));
        event_loop.stop();
    }
}
