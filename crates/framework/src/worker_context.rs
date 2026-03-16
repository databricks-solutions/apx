//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds the scheduler state for inline dispatch on the tokio
//! thread. It is created once per worker and shared via `Arc` with the
//! dispatch implementation.

use std::sync::Arc;

use pyo3::Py;

use crate::scheduler::driver::CachedTypes;
use crate::scheduler::queue::ReadyQueue;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to `AppSource::build()` which forwards it to the dispatch implementation.
pub struct WorkerContext {
    /// Pre-resolved Python type references.
    pub cached_types: Arc<CachedTypes>,
    /// Per-worker ready queue for suspended tasks.
    pub ready_queue: Arc<ReadyQueue>,
    /// Cached `loop.call_soon` bound method.
    pub call_soon: Py<pyo3::PyAny>,
    /// Python reference to the asyncio event loop (diagnostics, lifespan).
    #[expect(dead_code, reason = "read by lifespan protocol")]
    pub event_loop_ref: Py<pyo3::PyAny>,
}

impl std::fmt::Debug for WorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerContext").finish_non_exhaustive()
    }
}
