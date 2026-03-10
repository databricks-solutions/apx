//! [`Future`] — the foundational Rust-backed Python awaitable.
//!
//! Implements the Python awaitable protocol so that both asyncio and our Rust
//! coroutine driver can drive it.

use pyo3::prelude::*;
use tokio::sync::oneshot;

/// A Rust-backed Python awaitable.
///
/// Implements the Python awaitable protocol (`__await__` + `__next__`) and
/// can be resolved from Rust via [`set_result`](Future::set_result) or
/// [`set_exception`](Future::set_exception), or through a
/// [`oneshot::Sender`] returned by [`Future::with_channel`].
///
/// # Awaitable protocol
///
/// Python's `await` desugars to calling `__await__()` to get an iterator,
/// then repeatedly calling `__next__()` on it. When the result is ready,
/// `__next__` raises `StopIteration(value)`. Until then it yields `self`
/// so the Rust scheduler can classify and suspend on the future.
#[pyclass(module = "apx._core", weakref)]
pub struct Future {
    /// Oneshot receiver for results arriving from Rust.
    rx: Option<oneshot::Receiver<Py<PyAny>>>,
    /// Stored result (once resolved).
    inner_result: Option<PyResult<Py<PyAny>>>,
    /// Python callbacks registered via `add_done_callback`.
    wakers: Vec<Py<PyAny>>,
}

impl Future {
    /// Create a `Future` paired with a [`oneshot::Sender`] for resolution.
    ///
    /// The sender can be moved to any thread; sending a value through it
    /// will resolve the future on the next `__next__` poll.
    pub fn with_channel() -> (Self, oneshot::Sender<Py<PyAny>>) {
        let (tx, rx) = oneshot::channel();
        let future = Self {
            rx: Some(rx),
            inner_result: None,
            wakers: Vec::new(),
        };
        (future, tx)
    }

    /// Create a `Future` that is already resolved with the given value.
    pub fn resolved(value: Py<PyAny>) -> Self {
        Self {
            rx: None,
            inner_result: Some(Ok(value)),
            wakers: Vec::new(),
        }
    }

    /// Invoke all registered done callbacks with `self` as the argument.
    fn fire_wakers(&mut self, py: Python<'_>, slf: &Py<Self>) {
        for cb in self.wakers.drain(..) {
            // Best-effort: swallow exceptions from callbacks (matches asyncio behaviour).
            if let Err(e) = cb.call1(py, (slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        }
    }

    /// Raise `StopIteration(value)` or re-raise the stored exception.
    fn raise_result(py: Python<'_>, result: &PyResult<Py<PyAny>>) -> PyErr {
        match result {
            Ok(value) => pyo3::exceptions::PyStopIteration::new_err((value.clone_ref(py),)),
            Err(err) => err.clone_ref(py),
        }
    }
}

impl std::fmt::Debug for Future {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Future")
            .field("done", &self.inner_result.is_some())
            .field("wakers", &self.wakers.len())
            .finish()
    }
}

#[pymethods]
impl Future {
    /// Python awaitable protocol: return self as the iterator.
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Python iterator protocol (also needed for `__await__`).
    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Iterator protocol: poll for the result.
    ///
    /// - If the result is ready, raises `StopIteration(value)`.
    /// - If an exception was stored, re-raises it.
    /// - Otherwise yields `self` so the Rust scheduler can classify it as
    ///   `Future` and suspend (attach a done-callback) instead of
    ///   busy-looping on `YieldNone`.
    fn __next__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let mut this = slf.borrow_mut(py);

        // Already resolved — raise immediately.
        if let Some(ref result) = this.inner_result {
            return Err(Self::raise_result(py, result));
        }

        // Try to receive from the oneshot channel.
        if let Some(ref mut rx) = this.rx {
            match rx.try_recv() {
                Ok(value) => {
                    this.inner_result = Some(Ok(value.clone_ref(py)));
                    this.rx = None;
                    let stop = pyo3::exceptions::PyStopIteration::new_err((value,));
                    // Drop mutable borrow before firing wakers.
                    drop(this);
                    slf.borrow_mut(py).fire_wakers(py, &slf);
                    Err(stop)
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    // Not ready yet — yield self so the scheduler can suspend.
                    drop(this);
                    Ok(slf.into_any())
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    // Sender dropped without sending — treat as cancellation.
                    let err = pyo3::exceptions::PyRuntimeError::new_err(
                        "Future: sender dropped without producing a result",
                    );
                    this.inner_result = Some(Err(err.clone_ref(py)));
                    this.rx = None;
                    drop(this);
                    slf.borrow_mut(py).fire_wakers(py, &slf);
                    Err(err)
                }
            }
        } else {
            // No channel and no result — should not happen, but handle gracefully.
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Future: no channel and no result",
            ))
        }
    }

    /// Resolve the future with a value.
    ///
    /// Any registered done-callbacks are invoked immediately.
    fn set_result(slf: Py<Self>, py: Python<'_>, value: Py<PyAny>) -> PyResult<()> {
        {
            let mut this = slf.borrow_mut(py);
            if this.inner_result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            this.inner_result = Some(Ok(value));
            this.rx = None;
        }
        // Fire wakers outside the borrow.
        slf.borrow_mut(py).fire_wakers(py, &slf);
        Ok(())
    }

    /// Resolve the future with an exception.
    ///
    /// The exception object is stored and re-raised on the next `__next__` call.
    fn set_exception(slf: Py<Self>, py: Python<'_>, exc: Py<PyAny>) -> PyResult<()> {
        {
            let mut this = slf.borrow_mut(py);
            if this.inner_result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            let err = PyErr::from_value(exc.into_bound(py));
            this.inner_result = Some(Err(err));
            this.rx = None;
        }
        slf.borrow_mut(py).fire_wakers(py, &slf);
        Ok(())
    }

    /// Get the result if available. Raises if not yet resolved or if an exception was stored.
    #[pyo3(name = "result")]
    pub(crate) fn get_result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.inner_result {
            Some(Ok(value)) => Ok(value.clone_ref(py)),
            Some(Err(err)) => Err(err.clone_ref(py)),
            None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Future: result not yet available",
            )),
        }
    }

    /// Check whether the future has been resolved.
    pub(crate) fn done(&self) -> bool {
        self.inner_result.is_some()
    }

    /// Return the stored exception, if the future resolved with an error.
    pub(crate) fn exception(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        match &self.inner_result {
            Some(Err(err)) => Some(err.value(py).clone().unbind().into()),
            _ => None,
        }
    }

    /// Register a callback to be invoked when the future resolves.
    ///
    /// If the future is already resolved, the callback is invoked immediately.
    fn add_done_callback(slf: Py<Self>, py: Python<'_>, callback: Py<PyAny>) -> PyResult<()> {
        let done = slf.borrow(py).inner_result.is_some();
        if done {
            // Already done — fire immediately.
            if let Err(e) = callback.call1(py, (&slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        } else {
            slf.borrow_mut(py).wakers.push(callback);
        }
        Ok(())
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn with_channel_creates_pair() {
        let (future, _tx) = Future::with_channel();
        assert!(!future.done());
        assert!(future.rx.is_some());
        assert!(future.inner_result.is_none());
    }

    #[test]
    fn resolved_is_immediately_done() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            assert!(future.done());
            assert!(future.rx.is_none());
        });
    }

    #[test]
    fn debug_format() {
        let (future, _tx) = Future::with_channel();
        let dbg = format!("{future:?}");
        assert!(dbg.contains("Future"));
        assert!(dbg.contains("done: false"));
        assert!(dbg.contains("wakers: 0"));
    }

    #[test]
    fn double_set_result_errors() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            let slf = Py::new(py, future).unwrap();
            let err = Future::set_result(slf, py, py.None());
            assert!(err.is_err());
        });
    }
}
