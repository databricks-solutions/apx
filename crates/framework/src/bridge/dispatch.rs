//! Handler dispatch trait and shared dispatch infrastructure.
//!
//! The sole concrete implementation is [`super::asgi_dispatch::AsgiBridgeDispatch`] —
//! all routes are dispatched through the FastAPI ASGI app.

use crate::bridge::asgi::ScopeInterns;
use crate::error::AppError;
use crate::event_loop::{EventLoopHandle, SchedulerRefs};
use crate::route::BoundRoute;
use crate::transport::types::{InboundRequest, OutboundResponse};
use pyo3::Py;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Lifecycle-scoped state shared across all routes in a single worker.
pub struct AppState {
    /// Max request body size in bytes.
    pub max_body_limit: crate::route::BodyLimit,
    /// Handle to the persistent asyncio event loop.
    pub loop_handle: EventLoopHandle,
    /// Pre-interned Python strings for ASGI scope construction.
    pub scope_interns: Arc<ScopeInterns>,
    /// Pre-built scope template dict with fixed ASGI fields.
    pub scope_template: Arc<Py<pyo3::types::PyDict>>,
    /// Pre-built receive-event template dict with fixed ASGI fields.
    pub receive_template: Arc<Py<pyo3::types::PyDict>>,
    /// Cached `event_loop.create_task` bound method for Granian-style dispatch.
    pub create_task: Py<pyo3::PyAny>,
    /// Singleton ASGI error logger (stateless, reused across requests).
    pub error_logger: Py<pyo3::PyAny>,
    /// Scheduler refs for try-sync-first ASGI dispatch (`None` when not `RustNative`).
    pub scheduler_refs: Option<Arc<SchedulerRefs>>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        pyo3::Python::attach(|py| Self {
            max_body_limit: self.max_body_limit,
            loop_handle: self.loop_handle.clone(),
            scope_interns: Arc::clone(&self.scope_interns),
            scope_template: Arc::clone(&self.scope_template),
            receive_template: Arc::clone(&self.receive_template),
            create_task: self.create_task.clone_ref(py),
            error_logger: self.error_logger.clone_ref(py),
            scheduler_refs: self.scheduler_refs.clone(),
        })
    }
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
        let scope_interns = pyo3::Python::attach(ScopeInterns::new);
        let scope_template = pyo3::Python::attach(|py| {
            let d = pyo3::types::PyDict::new(py);
            d.unbind()
        });
        let receive_template = pyo3::Python::attach(|py| {
            crate::bridge::context_pool::build_receive_template(py).unwrap()
        });
        let (create_task, error_logger) = pyo3::Python::attach(|py| {
            let ct = event_loop
                .event_loop_ref()
                .getattr(py, "create_task")
                .unwrap();
            (ct, py.None())
        });
        let state = AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
            loop_handle: event_loop.handle().unwrap(),
            scope_interns: Arc::new(scope_interns),
            scope_template: Arc::new(scope_template),
            receive_template: Arc::new(receive_template),
            create_task,
            error_logger,
            scheduler_refs: None,
        };
        let dbg = format!("{state:?}");
        assert!(dbg.contains("AppState"));
        assert!(dbg.contains("max_body_limit"));
        event_loop.stop();
    }
}
