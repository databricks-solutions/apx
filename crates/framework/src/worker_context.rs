//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds the event loop handle, Python event loop reference,
//! and optional scheduler refs. It is created once per worker and shared
//! via `Arc` with the dispatch implementation.

use crate::event_loop::{EventLoopHandle, SchedulerRefs};
use pyo3::Py;
use std::sync::Arc;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to `AppSource::build()` which forwards it to the dispatch implementation.
pub struct WorkerContext {
    /// Handle for submitting coroutines to the persistent event loop.
    pub loop_handle: EventLoopHandle,
    /// Python reference to the asyncio event loop (diagnostics, lifespan).
    #[expect(dead_code, reason = "read by lifespan and try-sync-first dispatch")]
    pub event_loop_ref: Py<pyo3::PyAny>,
    /// Scheduler refs for Rust-native coroutine driving.
    #[expect(dead_code, reason = "read by try-sync-first inline dispatch path")]
    pub scheduler_refs: Arc<SchedulerRefs>,
}

impl std::fmt::Debug for WorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerContext")
            .field("loop_handle", &self.loop_handle)
            .finish_non_exhaustive()
    }
}
