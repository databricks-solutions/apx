//! Rust-driven coroutine scheduler.
//!
//! Replaces asyncio task scheduling for hot-path primitives while keeping
//! asyncio alive as a fallback for unhandled awaitables.

pub mod driver;
pub mod event_loop;
pub mod primitives;
pub mod queue;
pub mod task;

// ---------------------------------------------------------------------------
// Thread-local tokio runtime handle
// ---------------------------------------------------------------------------

use std::cell::RefCell;

thread_local! {
    /// Tokio runtime handle cached on the event loop thread.
    ///
    /// Set once during [`InlineEventLoop::init`] when the Rust
    /// scheduler is installed.
    static TOKIO_HANDLE: RefCell<Option<tokio::runtime::Handle>> = const { RefCell::new(None) };
}

/// Store the tokio runtime handle for the current (event loop) thread.
pub fn set_tokio_handle(handle: tokio::runtime::Handle) {
    TOKIO_HANDLE.with(|cell| *cell.borrow_mut() = Some(handle));
}

/// Run a closure with the thread-local tokio handle, if available.
pub fn with_tokio_handle<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&tokio::runtime::Handle) -> R,
{
    TOKIO_HANDLE.with(|cell| cell.borrow().as_ref().map(f))
}
