//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds the event loop handle and Python event loop reference.
//! It is created once per worker and shared via `Arc` with the dispatch
//! implementation.

use crate::event_loop::EventLoopHandle;
use pyo3::Py;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to `AppSource::build()` which forwards it to the dispatch implementation.
pub struct WorkerContext {
    /// Handle for submitting coroutines to the persistent event loop.
    pub loop_handle: EventLoopHandle,
    /// Python reference to the asyncio event loop (diagnostics, lifespan).
    #[expect(dead_code, reason = "read by lifespan protocol")]
    pub event_loop_ref: Py<pyo3::PyAny>,
}

impl std::fmt::Debug for WorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerContext")
            .field("loop_handle", &self.loop_handle)
            .finish_non_exhaustive()
    }
}
