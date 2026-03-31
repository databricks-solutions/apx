//! Python I/O interop — asyncio event loop lifecycle and coroutine submission.
//!
//! [`EventLoop`] is the composition root — it creates the asyncio reactor
//! and exposes cached Python callables for coroutine submission via
//! `call_soon_threadsafe(create_task, coro)`.

pub mod channel;
pub mod reactor;

use pyo3::prelude::*;

// ── EventLoop ────────────────────────────────────────────────────────────

/// Asyncio event loop lifecycle — owns the Reactor and exposes
/// cached Python callables for coroutine submission.
pub struct EventLoop {
    reactor: reactor::Reactor,
}

impl EventLoop {
    /// Initialize the event loop on the current thread.
    ///
    /// 1. Creates the asyncio reactor (event loop, thread).
    /// 2. Stores the tokio runtime handle in the thread-local.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization fails.
    pub fn init(py: Python<'_>, loop_policy: &str) -> Result<Self, String> {
        let reactor = reactor::Reactor::init(py, loop_policy)?;

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            set_tokio_handle(handle);
        }

        tracing::debug!(name: "apx.io.event_loop_initialized", "event loop initialized (asyncio delegation)");

        Ok(Self { reactor })
    }

    /// Cached `loop.call_soon_threadsafe` — the cross-thread submission primitive.
    pub fn call_soon_threadsafe(&self) -> &Py<PyAny> {
        self.reactor.call_soon_threadsafe()
    }

    /// Cached `loop.create_task` — creates a standard asyncio.Task.
    pub fn create_task(&self) -> &Py<PyAny> {
        self.reactor.create_task()
    }

    /// The Python asyncio event loop object.
    pub fn event_loop_py(&self) -> &Py<PyAny> {
        self.reactor.event_loop_py()
    }

    /// Shut down the event loop.
    pub fn shutdown(&self) {
        self.reactor.shutdown();
    }
}

crate::opaque_debug!(EventLoop);

// ── Thread-local tokio runtime handle ────────────────────────────────────

use std::cell::RefCell;

thread_local! {
    /// Tokio runtime handle cached on the event loop thread.
    ///
    /// Set once during [`EventLoop::init`] and on the asyncio thread
    /// during reactor init. `AsgiReceive` disconnect detection and
    /// `AsgiSend` backpressure handling need the runtime handle on
    /// the asyncio thread.
    static TOKIO_HANDLE: RefCell<Option<tokio::runtime::Handle>> = const { RefCell::new(None) };
}

/// Store the tokio runtime handle for the current thread.
pub fn set_tokio_handle(handle: tokio::runtime::Handle) {
    TOKIO_HANDLE.with(|cell| *cell.borrow_mut() = Some(handle));
}

/// Run a closure with a tokio runtime handle.
///
/// Checks the thread-local first (set via [`set_tokio_handle`]), then
/// falls back to [`tokio::runtime::Handle::try_current`].
pub fn with_tokio_handle<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&tokio::runtime::Handle) -> R,
{
    TOKIO_HANDLE.with(|cell| {
        if let Some(h) = cell.borrow().as_ref() {
            return Some(f(h));
        }
        tokio::runtime::Handle::try_current().ok().map(|h| f(&h))
    })
}
