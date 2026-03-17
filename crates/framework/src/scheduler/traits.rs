//! Trait boundaries for reactor and scheduler abstractions.

use std::sync::Arc;

use pyo3::prelude::*;

use crate::ffi::CoroutineOps;

use super::queue::ReadyQueue;

/// Asyncio event loop lifecycle management.
///
/// The reactor determines how and when asyncio callbacks are processed.
/// Implementations:
/// - `EventLoop`: dormant loop with explicit pump (current)
/// - Future: Rust-native event loop (rloop-style)
pub trait Reactor: Send + std::fmt::Debug {
    /// Access the Python event loop object.
    #[expect(dead_code, reason = "extension seam for future reactor swap")]
    fn event_loop_ref(&self) -> &Py<PyAny>;

    /// Access the cached `loop.call_soon_threadsafe` bound method.
    fn call_soon_threadsafe(&self) -> &Py<PyAny>;

    /// Shut down the reactor (cancel pending tasks, close loop).
    fn shutdown(&self);
}

/// Coroutine driving infrastructure.
///
/// The scheduler owns type classification state and the ready queue.
/// Implementations:
/// - `EventLoop`: inline driver on the tokio thread (current)
/// - Future: multi-thread scheduler for free-threaded Python
pub trait Scheduler: Send + std::fmt::Debug {
    /// Coroutine stepping and classification operations.
    fn coroutine_ops(&self) -> &Arc<dyn CoroutineOps>;

    /// Per-worker ready queue for task suspension and resumption.
    fn ready_queue(&self) -> &Arc<ReadyQueue>;
}
