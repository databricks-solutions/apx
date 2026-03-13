//! Persistent asyncio event loop (Granian-style architecture).
//!
//! One persistent event loop per worker, running `run_forever()` on a
//! dedicated Python thread. Handler coroutines are submitted via
//! `call_soon_threadsafe` and driven natively by asyncio.
//!
//! This fixes correctness issues with per-request event loops:
//! - `BackgroundTasks` persist after handler returns
//! - `contextvars` are maintained across middleware and handlers
//! - `get_running_loop()` always returns the correct loop

pub mod core;
pub mod handle;
pub mod queue;
pub mod scheduling;
// Thread-local event loop cache — available for future optimizations.
#[allow(
    dead_code,
    reason = "reserved for future spawn_blocking dispatch paths"
)]
pub mod thread_local;

pub use core::EventLoop;
pub use handle::EventLoopHandle;

use std::sync::Arc;

use crate::scheduler::driver::CachedTypes;
use crate::scheduler::queue::ReadyQueue;

/// Scheduler references for try-sync-first ASGI dispatch.
///
/// Cloned from [`queue::SchedulerState`] before it moves into the
/// [`queue::QueueDrainer`]. Allows ASGI dispatch to drive partially-
/// advanced coroutines through the Rust scheduler without `asyncio.Task`.
pub struct SchedulerRefs {
    /// Pre-resolved Python type references.
    pub(crate) cached_types: Arc<CachedTypes>,
    /// `loop.call_soon` bound method.
    pub(crate) call_soon: pyo3::Py<pyo3::PyAny>,
    /// `asyncio.ensure_future` function.
    pub(crate) ensure_future: pyo3::Py<pyo3::PyAny>,
    /// Shared ready queue for re-driving suspended tasks.
    pub(crate) ready_queue: Arc<ReadyQueue>,
}

impl Clone for SchedulerRefs {
    fn clone(&self) -> Self {
        pyo3::Python::attach(|py| Self {
            cached_types: Arc::clone(&self.cached_types),
            call_soon: self.call_soon.clone_ref(py),
            ensure_future: self.ensure_future.clone_ref(py),
            ready_queue: Arc::clone(&self.ready_queue),
        })
    }
}

impl std::fmt::Debug for SchedulerRefs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerRefs").finish_non_exhaustive()
    }
}
