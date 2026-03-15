//! Driver thread loop — the core execution unit of the driver pool.
//!
//! Each driver thread blocks with `py.detach(|| receiver.recv())`,
//! releasing the GIL while idle. When a [`DriverItem`] arrives, the thread
//! reacquires the GIL and drives the coroutine via `spawn_and_drive` or
//! `resume_task`.

use std::sync::Arc;

use pyo3::prelude::*;

use super::channel::{DriverItem, DriverReceiver, WorkItem};
use crate::scheduler::driver::{CachedTypes, resume_task, spawn_and_drive};
use crate::scheduler::queue::{ReadyQueue, ReadyTask};

/// Immutable state shared across all driver threads.
pub struct SharedDriverState {
    /// Pre-resolved Python type references.
    pub cached_types: Arc<CachedTypes>,
    /// Python event loop reference (for `_set_running_loop`).
    pub event_loop_ref: Py<PyAny>,
    /// `loop.call_soon_threadsafe` bound method.
    pub call_soon_threadsafe: Py<PyAny>,
    /// Shared ready queue.
    pub ready_queue: Arc<ReadyQueue>,
    /// Tokio runtime handle (for scheduler primitives).
    pub tokio_handle: Option<tokio::runtime::Handle>,
}

impl std::fmt::Debug for SharedDriverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedDriverState").finish_non_exhaustive()
    }
}

/// Configuration for a single driver thread.
pub struct DriverConfig {
    /// Thread identifier (monotonically increasing).
    pub id: usize,
    /// Receive side of the shared channel.
    pub receiver: DriverReceiver,
    /// Shared state (types, event loop ref, etc.).
    pub shared: Arc<SharedDriverState>,
}

impl std::fmt::Debug for DriverConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverConfig")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Run a driver thread's main loop.
///
/// Attaches to the Python interpreter, sets up asyncio's running loop
/// reference, and processes items from the channel until `Shutdown`.
pub fn run(config: DriverConfig) {
    Python::attach(|py| {
        setup_asyncio(py, &config.shared);
        if let Some(ref h) = config.shared.tokio_handle {
            crate::scheduler::set_tokio_handle(h.clone());
        }

        tracing::debug!(thread_id = config.id, "driver thread started");

        loop {
            // Release GIL while waiting for work.
            let item = py.detach(|| config.receiver.recv());
            match item {
                Ok(DriverItem::NewWork(work)) => drive_new(py, work, &config.shared),
                Ok(DriverItem::Resume(ready)) => drive_resume(py, ready, &config.shared),
                Ok(DriverItem::Shutdown) | Err(_) => {
                    tracing::debug!(thread_id = config.id, "driver thread shutting down");
                    break;
                }
            }
            // Yield GIL so the event loop thread can process I/O callbacks.
            // Without this, a continuous stream of channel items starves the
            // event loop — asyncio.Futures never resolve and coroutines deadlock.
            // Cost: one PyEval_SaveThread/RestoreThread cycle (~100-200ns).
            py.detach(|| {});
        }
    });
}

/// Set `asyncio.events._set_running_loop(loop)` so that
/// `asyncio.get_running_loop()` works on driver threads.
fn setup_asyncio(py: Python<'_>, shared: &SharedDriverState) {
    let events = match py.import(c"asyncio.events") {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "failed to import asyncio.events");
            return;
        }
    };
    if let Err(e) = events.call_method1(c"_set_running_loop", (&shared.event_loop_ref,)) {
        tracing::warn!(error = %e, "_set_running_loop failed");
    }
}

/// Drive a new coroutine from a `WorkItem`.
fn drive_new(py: Python<'_>, work: WorkItem, shared: &SharedDriverState) {
    let coro = match (work.builder)(py) {
        Ok(coro) => coro,
        Err(e) => {
            let _ = work.tx.send(Err(e));
            return;
        }
    };
    spawn_and_drive(
        py,
        coro,
        work.tx,
        &shared.cached_types,
        &shared.call_soon_threadsafe,
        &shared.ready_queue,
    );
}

/// Drive a resumed task from a `ReadyTask`.
fn drive_resume(py: Python<'_>, ready: ReadyTask, shared: &SharedDriverState) {
    if let Err(e) = resume_task(
        py,
        ready,
        &shared.cached_types,
        &shared.call_soon_threadsafe,
        &shared.ready_queue,
    ) {
        tracing::warn!(error = %e, "driver thread: resume failed");
    }
}
