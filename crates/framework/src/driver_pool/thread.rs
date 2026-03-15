//! Driver thread loop — builds scope dicts and forwards ready coroutines.
//!
//! Each driver thread blocks with `py.detach(|| receiver.recv())`,
//! releasing the GIL while idle. When a [`DriverItem::NewWork`] arrives,
//! the thread reacquires the GIL, calls the builder to construct the
//! coroutine, then pushes the ready coroutine to the event loop thread
//! via the stage-2 channel. **No coroutine driving happens here.**

use std::sync::Arc;

use pyo3::prelude::*;

use super::channel::{DriverItem, DriverReceiver, ReadyCoro, ReadyCoroSender};
use crate::event_loop::wake::WakeStrategy;

/// Immutable state shared across all driver threads.
pub struct SharedDriverState {
    /// Python event loop reference (for `_set_running_loop`).
    pub event_loop_ref: Py<PyAny>,
    /// Stage-2 channel sender (driver → event loop).
    pub event_loop_sender: ReadyCoroSender,
    /// Wake strategy for notifying the event loop after pushing a coro.
    pub wake: Arc<WakeStrategy>,
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
    /// Receive side of the shared channel (stage 1).
    pub receiver: DriverReceiver,
    /// Shared state (event loop ref, stage-2 sender, wake, etc.).
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
/// reference, and processes items from the stage-1 channel until `Shutdown`.
///
/// For each `NewWork` item:
/// 1. Acquire GIL (via `py.detach` return)
/// 2. Call builder to construct the Python coroutine (~2-5µs)
/// 3. Push `ReadyCoro` to the stage-2 channel
/// 4. Wake the event loop thread (pipe write or call_soon_threadsafe)
/// 5. Release GIL and wait for next item
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
                Ok(DriverItem::NewWork(work)) => {
                    let coro = match (work.builder)(py) {
                        Ok(coro) => coro,
                        Err(e) => {
                            let _ = work.tx.send(Err(e));
                            continue;
                        }
                    };
                    let ready = ReadyCoro { coro, tx: work.tx };
                    let _ = config.shared.event_loop_sender.send(ready);
                    config.shared.wake.wake();
                }
                Ok(DriverItem::Shutdown) | Err(_) => {
                    tracing::debug!(thread_id = config.id, "driver thread shutting down");
                    break;
                }
            }
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
