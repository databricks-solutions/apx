//! [`SchedulerTask`] — wraps a Python coroutine being driven by the scheduler.
//!
//! Maintains a coroutine stack (for inlined sub-coroutine driving), a result
//! future, and pending send/throw state.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use tokio::sync::oneshot;

use super::primitives::Future;
use crate::protocol::http::error::AppError;

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
    pub result_future: Py<Future>,
    /// Completion channel — consumed when the task completes or errors.
    result_tx: Option<oneshot::Sender<Result<Py<PyAny>, AppError>>>,
    /// Pending value to send to the active coroutine on next step.
    send_value: Option<Py<PyAny>>,
    /// Pending exception to throw into the active coroutine on next step.
    throw_error: Option<PyErr>,
    /// Cancellation state.
    cancelled: AtomicBool,
    /// Contextvars context — copied at task creation, entered before each
    /// drive cycle so that contextvars set in middleware propagate correctly.
    pub ctx: Option<Py<PyAny>>,
}

impl SchedulerTask {
    /// Wrap a coroutine and create a [`Future`] for its result.
    ///
    /// Copies the current contextvars context so that variables set in
    /// middleware propagate correctly into the handler coroutine.
    pub fn new(
        py: Python<'_>,
        coro: Py<PyAny>,
        result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    ) -> PyResult<Self> {
        let (fresh_future, _tx) = Future::with_channel();
        let result_future = Py::new(py, fresh_future)?;
        let ctx = copy_context(py);

        Ok(Self {
            coro_stack: vec![coro],
            result_future,
            result_tx: Some(result_tx),
            send_value: None,
            throw_error: None,
            cancelled: AtomicBool::new(false),
            ctx,
        })
    }

    /// Returns the root coroutine (the one originally passed to `new`).
    pub fn root_coro(&self, py: Python<'_>) -> Py<PyAny> {
        match self.coro_stack.first() {
            Some(c) => c.clone_ref(py),
            None => py.None(),
        }
    }

    /// Returns the top of the coroutine stack (the one currently being driven).
    ///
    /// # Errors
    ///
    /// Returns `PyRuntimeError` if the coroutine stack is empty (should never
    /// happen during normal driving).
    pub fn active_coro(&self) -> Result<&Py<PyAny>, PyErr> {
        self.coro_stack.last().ok_or_else(|| {
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
    pub fn take_send_value(&mut self) -> Option<Py<PyAny>> {
        self.send_value.take()
    }

    /// Take and return the pending throw error (consumed on use).
    pub fn take_throw_error(&mut self) -> Option<PyErr> {
        self.throw_error.take()
    }

    /// Take the completion channel (consumed when the task completes or errors).
    pub fn take_result_tx(&mut self) -> Option<oneshot::Sender<Result<Py<PyAny>, AppError>>> {
        self.result_tx.take()
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

/// Copy the current contextvars context via C-level `PyContext_CopyCurrent`.
///
/// Returns `None` if the copy fails (should not happen in practice).
#[expect(unsafe_code, reason = "FFI call to PyContext_CopyCurrent")]
fn copy_context(py: Python<'_>) -> Option<Py<PyAny>> {
    let ctx_ptr = unsafe { pyo3::ffi::PyContext_CopyCurrent() };
    if ctx_ptr.is_null() {
        // Clear any pending exception and return None.
        unsafe { pyo3::ffi::PyErr_Clear() };
        None
    } else {
        // Safety: PyContext_CopyCurrent returns a new reference on success.
        Some(unsafe { Bound::from_owned_ptr(py, ctx_ptr) }.unbind())
    }
}

impl std::fmt::Debug for SchedulerTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerTask")
            .field("stack_depth", &self.coro_stack.len())
            .field("has_result_tx", &self.result_tx.is_some())
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
    fn __await__(&self, py: Python<'_>) -> Py<Future> {
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
// TaskProxy — lightweight asyncio.Task-compatible proxy
// ---------------------------------------------------------------------------

/// Lightweight proxy installed as `asyncio.current_task()` during driving.
///
/// Implements enough of the `asyncio.Task` interface for Starlette's
/// `BaseHTTPMiddleware`, anyio's asyncio backend, and other middleware
/// that inspect the current task.
#[pyclass(module = "apx._core", weakref, freelist = 64)]
pub struct TaskProxy {
    result_future: Py<Future>,
    loop_ref: Py<PyAny>,
    coro: Py<PyAny>,
    cancelled: bool,
    name: String,
    /// The contextvars context copied at task creation.
    ctx: Option<Py<PyAny>>,
}

impl TaskProxy {
    /// Create a new proxy wrapping the given result future and event loop.
    pub fn new(
        result_future: Py<Future>,
        loop_ref: Py<PyAny>,
        coro: Py<PyAny>,
        ctx: Option<Py<PyAny>>,
    ) -> Self {
        Self {
            result_future,
            loop_ref,
            coro,
            cancelled: false,
            name: "TaskProxy".to_owned(),
            ctx,
        }
    }
}

impl std::fmt::Debug for TaskProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskProxy")
            .field("name", &self.name)
            .field("cancelled", &self.cancelled)
            .finish()
    }
}

#[pymethods]
impl TaskProxy {
    /// Register a done callback (delegates to the inner `Future`).
    #[pyo3(signature = (callback, *, context=None))]
    fn add_done_callback(
        &self,
        py: Python<'_>,
        callback: Py<PyAny>,
        context: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        let _ = context; // accepted for asyncio.Task API compat
        self.result_future
            .call_method1(py, c"add_done_callback", (callback,))?;
        Ok(())
    }

    /// Remove a done callback (stub — returns 0 removed).
    #[allow(clippy::unused_self, reason = "asyncio.Task API compatibility")]
    fn remove_done_callback(&self, _callback: Py<PyAny>) -> i32 {
        0
    }

    /// Request cancellation of the task.
    #[pyo3(signature = (msg=None))]
    fn cancel(&mut self, msg: Option<Py<PyAny>>) -> bool {
        let _ = msg;
        self.cancelled = true;
        true
    }

    /// Check whether the task has been cancelled.
    fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// Check whether the task is done.
    fn done(&self, py: Python<'_>) -> bool {
        self.result_future.borrow(py).done()
    }

    /// Get the result (delegates to the inner `Future`).
    fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.result_future.borrow(py).get_result(py)
    }

    /// Get the exception if the task failed, else `None`.
    fn exception(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.result_future.borrow(py).exception(py)
    }

    /// Get the task name.
    fn get_name(&self) -> &str {
        &self.name
    }

    /// Set the task name.
    fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// The event loop associated with this task.
    #[getter]
    fn _loop(&self, py: Python<'_>) -> Py<PyAny> {
        self.loop_ref.clone_ref(py)
    }

    // -- asyncio.Task internal attributes used by anyio's asyncio backend --

    /// Internal cancel flag (anyio reads this directly).
    #[getter]
    fn _must_cancel(&self) -> bool {
        self.cancelled
    }

    /// Internal waiter (anyio checks this for cancel propagation).
    #[getter]
    #[allow(clippy::unused_self, reason = "Python getter protocol requires &self")]
    fn _fut_waiter(&self) -> Option<Py<PyAny>> {
        None
    }

    /// Internal callbacks list (anyio reads this).
    #[getter]
    #[allow(clippy::unused_self, reason = "Python getter protocol requires &self")]
    fn _callbacks(&self) -> Vec<Py<PyAny>> {
        Vec::new()
    }

    /// Number of pending cancel requests (Python 3.11+).
    fn cancelling(&self) -> i32 {
        i32::from(self.cancelled)
    }

    /// Decrement cancel counter (Python 3.11+).
    fn uncancel(&mut self) -> i32 {
        self.cancelled = false;
        0
    }

    /// Return the wrapped coroutine.
    fn get_coro(&self, py: Python<'_>) -> Py<PyAny> {
        self.coro.clone_ref(py)
    }

    /// Return the task's contextvars context (copied at task creation).
    fn get_context(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        if let Some(ref ctx) = self.ctx {
            Ok(ctx.clone_ref(py))
        } else {
            // Fallback: copy current context if none was stored.
            let contextvars = py.import(c"contextvars")?;
            let ctx = contextvars.call_method0(c"copy_context")?;
            Ok(ctx.unbind())
        }
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

    fn dummy_tx() -> oneshot::Sender<Result<Py<PyAny>, AppError>> {
        oneshot::channel().0
    }

    #[test]
    fn new_task_has_single_coro() {
        crate::with_py(|py| {
            let coro = py.None();
            let task = SchedulerTask::new(py, coro, dummy_tx()).unwrap();
            assert_eq!(task.coro_stack.len(), 1);
            assert!(!task.is_cancelled());
        });
    }

    #[test]
    fn push_and_pop_coro_stack() {
        crate::with_py(|py| {
            let coro = py.None();
            let mut task = SchedulerTask::new(py, coro, dummy_tx()).unwrap();
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
            let mut task = SchedulerTask::new(py, coro, dummy_tx()).unwrap();

            assert!(task.take_send_value().is_none());

            let val = 42_i32.into_pyobject(py).unwrap().unbind().into_any();
            task.set_send_value(val);
            let taken = task.take_send_value();
            assert!(taken.is_some());
            let num: i32 = taken.unwrap().extract(py).unwrap();
            assert_eq!(num, 42);

            // Second take returns None.
            assert!(task.take_send_value().is_none());
        });
    }

    #[test]
    fn throw_error_round_trip() {
        crate::with_py(|py| {
            let coro = py.None();
            let mut task = SchedulerTask::new(py, coro, dummy_tx()).unwrap();

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
            let task = SchedulerTask::new(py, py.None(), dummy_tx()).unwrap();
            assert!(!task.is_cancelled());
            task.cancel_flag();
            assert!(task.is_cancelled());
        });
    }

    #[test]
    fn debug_format() {
        crate::with_py(|py| {
            let task = SchedulerTask::new(py, py.None(), dummy_tx()).unwrap();
            let dbg = format!("{task:?}");
            assert!(dbg.contains("SchedulerTask"));
            assert!(dbg.contains("stack_depth: 1"));
        });
    }
}
