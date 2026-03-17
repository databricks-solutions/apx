//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds the scheduler state for inline dispatch on the tokio
//! thread. It is created once per worker and shared via `Arc` with the
//! dispatch implementation.

use std::sync::Arc;

use pyo3::Py;

use crate::io::bridge::queue::ReadyQueue;
use crate::io::driver::ffi::CoroutineOps;
use crate::io::reactor::TaskOps;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to `AppSource::build()` which forwards it to the dispatch implementation.
pub struct WorkerContext {
    /// Coroutine stepping and classification operations.
    pub coroutine_ops: Arc<dyn CoroutineOps>,
    /// Per-worker ready queue for suspended tasks.
    pub ready_queue: Arc<ReadyQueue>,
    /// Cached `loop.call_soon_threadsafe` bound method.
    pub call_soon_threadsafe: Py<pyo3::PyAny>,
    /// Cached Python callables for scheduler task lifecycle.
    pub task_ops: TaskOps,
}

impl std::fmt::Debug for WorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerContext").finish_non_exhaustive()
    }
}
