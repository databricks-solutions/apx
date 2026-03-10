//! [`SchedulerTask`] — wraps a Python coroutine being driven by the scheduler.
//!
//! Maintains a coroutine stack (for inlined sub-coroutine driving), a result
//! future, and pending send/throw state.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;

use super::primitives::RustFuture;

// ---------------------------------------------------------------------------
// SchedulerTask
// ---------------------------------------------------------------------------

/// A coroutine being driven by the Rust scheduler.
///
/// Instead of wrapping each coroutine in an `asyncio.Task`, the scheduler
/// drives it directly via [`super::driver::drive_task`]. Sub-coroutines are
/// pushed onto an internal stack and driven inline — no extra task objects.
#[pyclass(module = "apx._core", weakref)]
pub struct SchedulerTask {
    /// Stack of coroutines (top = active). When a sub-coroutine is yielded,
    /// it is pushed; when it completes, it is popped and the result is sent
    /// to the parent.
    coro_stack: Vec<Py<PyAny>>,
    /// Result future — where the final result goes.
    pub result_future: Py<RustFuture>,
    /// Pending value to send to the active coroutine on next step.
    send_value: Option<Py<PyAny>>,
    /// Pending exception to throw into the active coroutine on next step.
    throw_error: Option<PyErr>,
    /// Cancellation state.
    cancelled: AtomicBool,
}

impl SchedulerTask {
    /// Wrap a coroutine and create a [`RustFuture`] for its result.
    pub fn new(py: Python<'_>, coro: Py<PyAny>) -> PyResult<Self> {
        let (fresh_future, _tx) = RustFuture::with_channel();
        let result_future = Py::new(py, fresh_future)?;

        Ok(Self {
            coro_stack: vec![coro],
            result_future,
            send_value: None,
            throw_error: None,
            cancelled: AtomicBool::new(false),
        })
    }

    /// Returns the top of the coroutine stack (the one currently being driven).
    ///
    /// # Errors
    ///
    /// Returns `PyRuntimeError` if the coroutine stack is empty (should never
    /// happen during normal driving).
    pub fn active_coro<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.coro_stack
            .last()
            .map(|c| c.bind(py).clone())
            .ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("SchedulerTask: coroutine stack is empty")
            })
    }

    /// Push a sub-coroutine onto the stack.
    pub fn push_coro(&mut self, coro: Py<PyAny>) {
        self.coro_stack.push(coro);
    }

    /// Pop the top coroutine. Returns `true` if there is still a parent
    /// coroutine to resume.
    pub fn pop_coro(&mut self) -> bool {
        self.coro_stack.pop();
        !self.coro_stack.is_empty()
    }

    /// Set the value to send on the next step.
    pub fn set_send_value(&mut self, value: Py<PyAny>) {
        self.send_value = Some(value);
    }

    /// Set the exception to throw on the next step.
    pub fn set_throw_error(&mut self, err: PyErr) {
        self.throw_error = Some(err);
    }

    /// Take and return the pending send value (consumed on use).
    pub fn take_send_value<'py>(&mut self, py: Python<'py>) -> Option<Bound<'py, PyAny>> {
        self.send_value.take().map(|v| v.into_bound(py))
    }

    /// Take and return the pending throw error (consumed on use).
    pub fn take_throw_error(&mut self) -> Option<PyErr> {
        self.throw_error.take()
    }

    /// Resolve the result future with a value.
    pub fn complete(&self, py: Python<'_>, value: Py<PyAny>) {
        let fut = self.result_future.bind(py);
        if let Err(e) = fut.call_method1(c"set_result", (value,)) {
            tracing::warn!(error = %e, "SchedulerTask: failed to set result");
        }
    }

    /// Resolve the result future with an exception.
    pub fn fail(&self, py: Python<'_>, err: PyErr) {
        let exc = err.value(py).clone().unbind();
        let fut = self.result_future.bind(py);
        if let Err(e) = fut.call_method1(c"set_exception", (exc,)) {
            tracing::warn!(error = %e, "SchedulerTask: failed to set exception");
        }
    }

    /// Set the cancellation flag.
    pub fn cancel_flag(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check whether the task has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for SchedulerTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerTask")
            .field("stack_depth", &self.coro_stack.len())
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .field("has_send_value", &self.send_value.is_some())
            .field("has_throw_error", &self.throw_error.is_some())
            .finish()
    }
}

#[pymethods]
impl SchedulerTask {
    /// Python awaitable protocol: return the result future (which itself
    /// implements `__await__` / `__iter__` / `__next__`).
    fn __await__(&self, py: Python<'_>) -> Py<RustFuture> {
        self.result_future.clone_ref(py)
    }

    /// Cancel the task.
    fn cancel(&self) {
        self.cancel_flag();
    }

    /// Check whether the result future is done.
    fn done(&self, py: Python<'_>) -> PyResult<bool> {
        let val = self.result_future.bind(py).call_method0(c"done")?;
        val.extract()
    }

    /// Get the result from the result future.
    fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let val = self.result_future.bind(py).call_method0(c"result")?;
        Ok(val.unbind())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn new_task_has_single_coro() {
        crate::with_py(|py| {
            let coro = py.None();
            let task = SchedulerTask::new(py, coro).unwrap();
            assert_eq!(task.coro_stack.len(), 1);
            assert!(!task.is_cancelled());
        });
    }

    #[test]
    fn push_and_pop_coro_stack() {
        crate::with_py(|py| {
            let coro = py.None();
            let mut task = SchedulerTask::new(py, coro).unwrap();
            assert_eq!(task.coro_stack.len(), 1);

            task.push_coro(py.None());
            assert_eq!(task.coro_stack.len(), 2);

            // Pop sub-coroutine — parent still exists.
            assert!(task.pop_coro());
            assert_eq!(task.coro_stack.len(), 1);

            // Pop top-level — nothing left.
            assert!(!task.pop_coro());
            assert!(task.coro_stack.is_empty());
        });
    }

    #[test]
    fn send_value_round_trip() {
        crate::with_py(|py| {
            let coro = py.None();
            let mut task = SchedulerTask::new(py, coro).unwrap();

            assert!(task.take_send_value(py).is_none());

            let val = 42_i32.into_pyobject(py).unwrap().unbind().into_any();
            task.set_send_value(val);
            let taken = task.take_send_value(py);
            assert!(taken.is_some());
            let num: i32 = taken.unwrap().extract().unwrap();
            assert_eq!(num, 42);

            // Second take returns None.
            assert!(task.take_send_value(py).is_none());
        });
    }

    #[test]
    fn throw_error_round_trip() {
        crate::with_py(|py| {
            let coro = py.None();
            let mut task = SchedulerTask::new(py, coro).unwrap();

            assert!(task.take_throw_error().is_none());

            let err = pyo3::exceptions::PyValueError::new_err("boom");
            task.set_throw_error(err);
            let taken = task.take_throw_error();
            assert!(taken.is_some());

            // Verify it is a ValueError.
            let e = taken.unwrap();
            assert!(e.is_instance_of::<pyo3::exceptions::PyValueError>(py));

            // Second take returns None.
            assert!(task.take_throw_error().is_none());
        });
    }

    #[test]
    fn cancel_flag() {
        crate::with_py(|py| {
            let task = SchedulerTask::new(py, py.None()).unwrap();
            assert!(!task.is_cancelled());
            task.cancel_flag();
            assert!(task.is_cancelled());
        });
    }

    #[test]
    fn debug_format() {
        crate::with_py(|py| {
            let task = SchedulerTask::new(py, py.None()).unwrap();
            let dbg = format!("{task:?}");
            assert!(dbg.contains("SchedulerTask"));
            assert!(dbg.contains("stack_depth: 1"));
        });
    }
}
