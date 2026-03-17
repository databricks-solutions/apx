//! [`Future`] — the foundational Rust-backed Python awaitable.
//!
//! Implements the Python awaitable protocol so that both asyncio and our Rust
//! coroutine driver can drive it.

use std::sync::Mutex;

use pyo3::prelude::*;

/// Internal mutable state, protected by a Mutex.
struct FutureInner {
    result: Option<PyResult<Py<PyAny>>>,
    wakers: Vec<Py<PyAny>>,
}

/// A Rust-backed Python awaitable.
///
/// Implements the Python awaitable protocol (`__await__` + `__next__`) and
/// can be resolved from Rust via [`set_result`](Future::set_result) or
/// [`set_exception`](Future::set_exception).
///
/// Uses `#[pyclass(frozen)]` with interior mutability (`Mutex`) to avoid
/// PyO3's `RefCell`-based borrow tracking entirely. The Mutex never contends
/// in practice — the GIL serializes all access.
///
/// # Awaitable protocol
///
/// Python's `await` desugars to calling `__await__()` to get an iterator,
/// then repeatedly calling `__next__()` on it. When the result is ready,
/// `__next__` raises `StopIteration(value)`. Until then it yields `self`
/// so the Rust scheduler can classify and suspend on the future.
#[pyclass(frozen, module = "apx._core", weakref)]
pub struct Future {
    inner: Mutex<FutureInner>,
}

impl Future {
    /// Create an unresolved `Future`.
    ///
    /// Resolve later via [`set_result`] or [`set_exception`], both of
    /// which fire done-callbacks immediately.
    pub fn pending() -> Self {
        Self {
            inner: Mutex::new(FutureInner {
                result: None,
                wakers: Vec::new(),
            }),
        }
    }

    /// Create a `Future` that is already resolved with the given value.
    pub fn resolved(value: Py<PyAny>) -> Self {
        Self {
            inner: Mutex::new(FutureInner {
                result: Some(Ok(value)),
                wakers: Vec::new(),
            }),
        }
    }

    /// Raise `StopIteration(value)` or re-raise the stored exception.
    fn raise_result(py: Python<'_>, result: &PyResult<Py<PyAny>>) -> PyErr {
        match result {
            Ok(value) => pyo3::exceptions::PyStopIteration::new_err((value.clone_ref(py),)),
            Err(err) => err.clone_ref(py),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FutureInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for Future {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("Future")
            .field("done", &inner.result.is_some())
            .field("wakers", &inner.wakers.len())
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
        let inner = slf.get().lock();
        if let Some(ref result) = inner.result {
            return Err(Self::raise_result(py, result));
        }
        drop(inner);
        Ok(slf.into_any())
    }

    /// Resolve the future with a value.
    ///
    /// Any registered done-callbacks are invoked immediately.
    pub(crate) fn set_result(slf: Py<Self>, py: Python<'_>, value: Py<PyAny>) -> PyResult<()> {
        let wakers = {
            let mut inner = slf.get().lock();
            if inner.result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            inner.result = Some(Ok(value));
            std::mem::take(&mut inner.wakers)
        };
        // Fire wakers outside the lock — callbacks may call done()/result().
        for cb in wakers {
            if let Err(e) = cb.call1(py, (&slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        }
        Ok(())
    }

    /// Resolve the future with an exception.
    ///
    /// The exception object is stored and re-raised on the next `__next__` call.
    fn set_exception(slf: Py<Self>, py: Python<'_>, exc: Py<PyAny>) -> PyResult<()> {
        let wakers = {
            let mut inner = slf.get().lock();
            if inner.result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            let err = PyErr::from_value(exc.into_bound(py));
            inner.result = Some(Err(err));
            std::mem::take(&mut inner.wakers)
        };
        for cb in wakers {
            if let Err(e) = cb.call1(py, (&slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        }
        Ok(())
    }

    /// Get the result if available. Raises if not yet resolved or if an exception was stored.
    #[pyo3(name = "result")]
    pub(crate) fn get_result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let inner = self.lock();
        match &inner.result {
            Some(Ok(value)) => Ok(value.clone_ref(py)),
            Some(Err(err)) => Err(err.clone_ref(py)),
            None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Future: result not yet available",
            )),
        }
    }

    /// Check whether the future has been resolved.
    pub(crate) fn done(&self) -> bool {
        self.lock().result.is_some()
    }

    /// Return the stored exception, if the future resolved with an error.
    pub(crate) fn exception(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        let inner = self.lock();
        match &inner.result {
            Some(Err(err)) => Some(err.value(py).clone().unbind().into()),
            _ => None,
        }
    }

    /// Register a callback to be invoked when the future resolves.
    ///
    /// If the future is already resolved, the callback is invoked immediately.
    fn add_done_callback(slf: Py<Self>, py: Python<'_>, callback: Py<PyAny>) -> PyResult<()> {
        let fire_now = {
            let mut inner = slf.get().lock();
            if inner.result.is_some() {
                true
            } else {
                inner.wakers.push(callback.clone_ref(py));
                false
            }
        };
        if fire_now && let Err(e) = callback.call1(py, (&slf,)) {
            tracing::warn!(error = %e, "Future done-callback raised");
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
    fn resolved_is_immediately_done() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            assert!(future.done());
        });
    }

    #[test]
    fn debug_format() {
        let future = Future::pending();
        let dbg = format!("{future:?}");
        assert!(dbg.contains("Future"));
        assert!(dbg.contains("done: false"));
        assert!(dbg.contains("wakers: 0"));
    }

    #[test]
    fn pending_is_not_done() {
        let future = Future::pending();
        assert!(!future.done());
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

    #[test]
    fn resolved_callback_fires_immediately() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            let slf = Py::new(py, future).unwrap();
            py.run(c"_cb_called = False", None, None).unwrap();
            let cb = py
                .eval(
                    c"lambda fut: globals().__setitem__('_cb_called', True)",
                    None,
                    None,
                )
                .unwrap()
                .unbind();
            Future::add_done_callback(slf, py, cb).unwrap();
            let called: bool = py
                .eval(c"_cb_called", None, None)
                .unwrap()
                .extract()
                .unwrap();
            assert!(
                called,
                "callback should fire immediately on resolved future"
            );
        });
    }

    #[test]
    fn exception_propagates_on_next() {
        crate::with_py(|py| {
            let future = Future::pending();
            let slf = Py::new(py, future).unwrap();
            let exc = pyo3::exceptions::PyValueError::new_err("boom");
            Future::set_exception(slf.clone_ref(py), py, exc.value(py).clone().unbind().into())
                .unwrap();
            // __next__ should re-raise the stored exception
            let result = Future::__next__(slf, py);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    #[test]
    fn multiple_wakers_all_fire() {
        crate::with_py(|py| {
            let future = Future::pending();
            let slf = Py::new(py, future).unwrap();
            py.run(c"_fire_count = 0", None, None).unwrap();
            let cb = py
                .eval(
                    c"lambda fut: globals().__setitem__('_fire_count', globals()['_fire_count'] + 1)",
                    None,
                    None,
                )
                .unwrap()
                .unbind();
            Future::add_done_callback(slf.clone_ref(py), py, cb.clone_ref(py)).unwrap();
            Future::add_done_callback(slf.clone_ref(py), py, cb.clone_ref(py)).unwrap();
            Future::add_done_callback(slf.clone_ref(py), py, cb).unwrap();
            Future::set_result(slf, py, py.None()).unwrap();
            let count: i32 = py
                .eval(c"_fire_count", None, None)
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(count, 3);
        });
    }

    #[test]
    fn await_protocol_chain() {
        crate::with_py(|py| {
            let future = Future::pending();
            let slf = Py::new(py, future).unwrap();
            let awaited = Future::__await__(slf.clone_ref(py));
            assert!(awaited.is(&slf));
            let itered = Future::__iter__(slf.clone_ref(py));
            assert!(itered.is(&slf));
            // __next__ on pending should return Ok(self)
            let next_result = Future::__next__(slf.clone_ref(py), py).unwrap();
            assert!(next_result.is(slf.clone_ref(py).into_any()));
        });
    }
}
