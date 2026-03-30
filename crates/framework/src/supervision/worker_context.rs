//! Shared worker infrastructure passed to every dispatch strategy.
//!
//! `WorkerContext` holds the dispatch pipeline for the 3-thread
//! architecture plus legacy asyncio submission callables used by the
//! WebSocket path (which still submits via `call_soon_threadsafe`).

use crate::io::channel::DispatchPipeline;
use pyo3::Py;
use std::sync::Arc;

/// Shared infrastructure available to all dispatch strategies.
///
/// Created once per worker in `run_worker`, wrapped in `Arc`, and passed
/// to the dispatch implementation.
pub struct WorkerContext {
    /// 3-thread dispatch pipeline (channels + wakeup).
    pub pipeline: Arc<DispatchPipeline>,
    /// Cached `loop.call_soon_threadsafe` — used by WS dispatch path.
    pub call_soon_threadsafe: Py<pyo3::PyAny>,
    /// Cached `_bridge.launch` — used by WS dispatch path.
    pub launch_fn: Py<pyo3::PyAny>,
}

crate::opaque_debug!(WorkerContext);
