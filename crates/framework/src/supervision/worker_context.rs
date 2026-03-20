//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds cached Python callables for asyncio delegation.
//! It is created once per worker and shared via `Arc` with the dispatch
//! implementation.

use pyo3::Py;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to `AppSource::build()` which forwards it to the dispatch implementation.
pub struct WorkerContext {
    /// Cached `loop.call_soon_threadsafe` for cross-thread submission.
    pub call_soon_threadsafe: Py<pyo3::PyAny>,
    /// Cached `loop.create_task` for asyncio task creation.
    pub create_task: Py<pyo3::PyAny>,
    /// Cached `_bridge.launch` — creates ASGI task on the asyncio thread.
    pub launch_fn: Py<pyo3::PyAny>,
}

impl std::fmt::Debug for WorkerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerContext").finish_non_exhaustive()
    }
}
