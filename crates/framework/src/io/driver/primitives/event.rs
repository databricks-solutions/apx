//! [`Event`] and [`EventWaiter`] — async event flag (wraps `tokio::sync::Notify` pattern).

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;

use super::future::Future;

/// A Rust-backed async event flag, analogous to `asyncio.Event`.
///
/// `wait()` returns a [`EventWaiter`] that wraps a [`Future`].
/// When `set()` is called, all pending waiter futures are resolved,
/// causing the Rust scheduler to resume waiting coroutines via
/// done-callbacks instead of busy-polling.
#[pyclass(module = "apx._core")]
pub struct Event {
    is_set: AtomicBool,
    /// Pending waiter futures — resolved via `Future::set_result()` when `set()` is called.
    pending: std::sync::Mutex<Vec<Py<Future>>>,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("set", &self.is_set.load(Ordering::Relaxed))
            .finish()
    }
}

#[pymethods]
impl Event {
    #[new]
    pub(crate) fn new() -> Self {
        Self {
            is_set: AtomicBool::new(false),
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Set the event flag and resolve all pending waiter futures.
    fn set(&self) {
        self.is_set.store(true, Ordering::Release);
        let waiters = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pending)
        };
        if !waiters.is_empty() {
            Python::attach(|py| {
                for fut in waiters {
                    let _ = Future::set_result(fut, py, py.None());
                }
            });
        }
    }

    /// Check whether the event is currently set.
    pub(crate) fn is_set(&self) -> bool {
        self.is_set.load(Ordering::Acquire)
    }

    /// Reset the event flag.
    fn clear(&self) {
        self.is_set.store(false, Ordering::Release);
    }

    /// Return an awaitable that resolves when the event is set.
    fn wait(&self, py: Python<'_>) -> PyResult<EventWaiter> {
        if self.is_set.load(Ordering::Acquire) {
            let inner = Py::new(py, Future::resolved(py.None()))?;
            return Ok(EventWaiter { inner });
        }
        let inner = Py::new(py, Future::pending())?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        // Double-check after acquiring lock — event may have been set.
        if self.is_set.load(Ordering::Acquire) {
            drop(pending);
            let _ = Future::set_result(inner.clone_ref(py), py, py.None());
        } else {
            pending.push(inner.clone_ref(py));
        }
        Ok(EventWaiter { inner })
    }
}

/// Awaitable returned by [`Event::wait`].
///
/// Wraps a [`Future`] that resolves when the parent event is set.
/// The scheduler can classify and suspend on the inner future properly.
#[pyclass(module = "apx._core")]
pub struct EventWaiter {
    inner: Py<Future>,
}

impl std::fmt::Debug for EventWaiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventWaiter").finish_non_exhaustive()
    }
}

#[pymethods]
impl EventWaiter {
    /// Python awaitable protocol: delegate to the inner Future.
    fn __await__(&self, py: Python<'_>) -> Py<Future> {
        self.inner.clone_ref(py)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_starts_unset() {
        let event = Event::new();
        assert!(!event.is_set());
    }

    #[test]
    fn event_set_and_clear() {
        // Event::set() calls Python::attach internally to resolve waiters.
        crate::with_py(|_py| {
            let event = Event::new();
            event.set();
            assert!(event.is_set());
            event.clear();
            assert!(!event.is_set());
        });
    }
}
