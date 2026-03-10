//! [`TaskGroupCore`] — Rust-backed state for anyio TaskGroup.
//!
//! The Python `ApxTaskGroup` class handles the async context manager protocol
//! and `start_soon`/`start` methods. The Rust core handles child task
//! completion tracking and exception collection.

use pyo3::prelude::*;

use super::super::primitives::Future;

/// Rust-backed task group core.
///
/// Tracks the number of pending child tasks, collects exceptions from
/// failed children, and resolves a completion future when all children
/// are done.
#[pyclass(module = "apx._core")]
pub struct TaskGroupCore {
    /// Number of pending child tasks.
    pending_count: usize,
    /// Collected exceptions from failed children.
    exceptions: Vec<Py<PyAny>>,
    /// Completion future — resolved when all children complete.
    completion_future: Option<Py<Future>>,
    /// Sender for the completion future.
    completion_tx: Option<tokio::sync::oneshot::Sender<Py<PyAny>>>,
}

impl std::fmt::Debug for TaskGroupCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskGroupCore")
            .field("pending", &self.pending_count)
            .field("exceptions", &self.exceptions.len())
            .finish()
    }
}

#[pymethods]
impl TaskGroupCore {
    #[new]
    pub(crate) fn new(py: Python<'_>) -> PyResult<Self> {
        let (future, tx) = Future::with_channel();
        let completion_future = Py::new(py, future)?;
        Ok(Self {
            pending_count: 0,
            exceptions: Vec::new(),
            completion_future: Some(completion_future),
            completion_tx: Some(tx),
        })
    }

    /// Increment the pending child count.
    fn child_spawned(&mut self) {
        self.pending_count += 1;
    }

    /// Called when a child task completes (successfully or with an error).
    ///
    /// If `exception` is `Some`, the exception is collected. When all children
    /// are done, the completion future is resolved.
    #[pyo3(signature = (exception=None))]
    fn child_completed(&mut self, py: Python<'_>, exception: Option<Py<PyAny>>) {
        if let Some(exc) = exception {
            self.exceptions.push(exc);
        }
        self.pending_count = self.pending_count.saturating_sub(1);
        if self.pending_count == 0 {
            self.resolve_completion(py);
        }
    }

    /// Return the completion future (awaited by `__aexit__`).
    fn get_completion_future(&self, py: Python<'_>) -> Option<Py<Future>> {
        self.completion_future.as_ref().map(|f| f.clone_ref(py))
    }

    /// Return collected exceptions.
    fn get_exceptions(&self, py: Python<'_>) -> Vec<Py<PyAny>> {
        self.exceptions.iter().map(|e| e.clone_ref(py)).collect()
    }

    /// Check if there are any pending children.
    pub(crate) fn has_pending(&self) -> bool {
        self.pending_count > 0
    }

    /// Check if there were any exceptions.
    fn has_exceptions(&self) -> bool {
        !self.exceptions.is_empty()
    }

    /// Resolve completion immediately (used when no children were spawned).
    fn resolve_if_empty(&mut self, py: Python<'_>) {
        if self.pending_count == 0 {
            self.resolve_completion(py);
        }
    }
}

impl TaskGroupCore {
    /// Resolve the completion future.
    fn resolve_completion(&mut self, py: Python<'_>) {
        if let Some(tx) = self.completion_tx.take() {
            let _ = tx.send(py.None());
        }
    }
}

/// Python source for the `ApxTaskGroup` class.
///
/// Implements anyio's TaskGroup interface using `TaskGroupCore` for tracking
/// and the event loop's `create_task` for child task spawning.
#[expect(dead_code, reason = "consumed in Phase 5 via anyio_backend rewrite")]
pub const TASK_GROUP_GLUE: &str = r#"
import asyncio
import sys

class ApxTaskGroup:
    """TaskGroup compatible with anyio's interface."""

    def __init__(self, core, cancel_scope):
        self._core = core
        self.cancel_scope = cancel_scope
        self._host_task = None
        self._tasks = []

    async def __aenter__(self):
        self._host_task = asyncio.current_task()
        self.cancel_scope.__enter__()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        try:
            if self._core.has_pending():
                fut = self._core.get_completion_future()
                if fut is not None:
                    await fut
            else:
                self._core.resolve_if_empty()

            exceptions = self._core.get_exceptions()
            if exceptions:
                # Cancel the scope on child failure
                self.cancel_scope.cancel()
                if exc_val is not None:
                    exceptions.append(exc_val)
                if len(exceptions) == 1:
                    raise exceptions[0]
                raise BaseExceptionGroup("multiple child errors", exceptions)
        finally:
            self.cancel_scope.__exit__(*sys.exc_info())

        return False

    def start_soon(self, func, *args, name=None):
        coro = func(*args)
        self._core.child_spawned()

        loop = asyncio.get_running_loop()
        task = loop.create_task(coro)
        if name:
            task.set_name(name)
        self._tasks.append(task)

        # When child completes, notify the core
        def _on_done(t):
            exc = None
            if not t.cancelled():
                exc = t.exception()
            if exc is not None:
                self._core.child_completed(exc)
            else:
                self._core.child_completed()

        task.add_done_callback(_on_done)

    async def start(self, func, *args, name=None):
        task_status_future = asyncio.get_running_loop().create_future()

        class _TaskStatus:
            def __init__(self):
                self._started = False

            def started(self, value=None):
                if self._started:
                    raise RuntimeError("started() called twice")
                self._started = True
                task_status_future.set_result(value)

        async def _wrapper():
            return await func(*args, task_status=_TaskStatus())

        self.start_soon(_wrapper, name=name)
        return await task_status_future
"#;

/// Evaluate the task group Python glue and return the module dict.
pub fn eval_task_group_glue(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let code = std::ffi::CString::new(
        r#"
import asyncio
import sys

class ApxTaskGroup:
    """TaskGroup compatible with anyio's interface."""

    def __init__(self, core, cancel_scope):
        self._core = core
        self.cancel_scope = cancel_scope
        self._host_task = None
        self._tasks = []

    async def __aenter__(self):
        self._host_task = asyncio.current_task()
        self.cancel_scope.__enter__()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        try:
            if self._core.has_pending():
                fut = self._core.get_completion_future()
                if fut is not None:
                    await fut
            else:
                self._core.resolve_if_empty()

            exceptions = self._core.get_exceptions()
            if exceptions:
                self.cancel_scope.cancel()
                if exc_val is not None:
                    exceptions.append(exc_val)
                if len(exceptions) == 1:
                    raise exceptions[0]
                raise BaseExceptionGroup("multiple child errors", exceptions)
        finally:
            self.cancel_scope.__exit__(*sys.exc_info())

        return False

    def start_soon(self, func, *args, name=None):
        coro = func(*args)
        self._core.child_spawned()

        loop = asyncio.get_running_loop()
        task = loop.create_task(coro)
        if name:
            task.set_name(name)
        self._tasks.append(task)

        def _on_done(t):
            exc = None
            if not t.cancelled():
                exc = t.exception()
            if exc is not None:
                self._core.child_completed(exc)
            else:
                self._core.child_completed()

        task.add_done_callback(_on_done)

    async def start(self, func, *args, name=None):
        task_status_future = asyncio.get_running_loop().create_future()

        class _TaskStatus:
            def __init__(self):
                self._started = False

            def started(self, value=None):
                if self._started:
                    raise RuntimeError("started() called twice")
                self._started = True
                task_status_future.set_result(value)

        async def _wrapper():
            return await func(*args, task_status=_TaskStatus())

        self.start_soon(_wrapper, name=name)
        return await task_status_future
"#,
    )?;

    let locals = pyo3::types::PyDict::new(py);
    py.run(&code, None, Some(&locals))?;
    Ok(locals.unbind().into_any())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn task_group_core_defaults() {
        crate::with_py(|py| {
            let core = TaskGroupCore::new(py).unwrap();
            assert!(!core.has_pending());
            assert!(!core.has_exceptions());
        });
    }

    #[test]
    fn task_group_core_child_lifecycle() {
        crate::with_py(|py| {
            let mut core = TaskGroupCore::new(py).unwrap();
            core.child_spawned();
            core.child_spawned();
            assert!(core.has_pending());

            core.child_completed(py, None);
            assert!(core.has_pending());

            core.child_completed(py, None);
            assert!(!core.has_pending());
            assert!(!core.has_exceptions());
        });
    }

    #[test]
    fn task_group_core_child_exception() {
        crate::with_py(|py| {
            let mut core = TaskGroupCore::new(py).unwrap();
            core.child_spawned();

            let exc = pyo3::exceptions::PyValueError::new_err("child failed");
            core.child_completed(py, Some(exc.value(py).clone().unbind().into()));
            assert!(core.has_exceptions());
            assert_eq!(core.get_exceptions(py).len(), 1);
        });
    }

    #[test]
    fn task_group_core_debug() {
        crate::with_py(|py| {
            let core = TaskGroupCore::new(py).unwrap();
            let dbg = format!("{core:?}");
            assert!(dbg.contains("TaskGroupCore"));
            assert!(dbg.contains("pending: 0"));
        });
    }

    #[test]
    fn task_group_glue_evaluates() {
        crate::with_py(|py| {
            let locals = eval_task_group_glue(py).unwrap();
            let locals = locals
                .into_bound(py)
                .cast_into::<pyo3::types::PyDict>()
                .unwrap();
            assert!(locals.get_item("ApxTaskGroup").unwrap().is_some());
        });
    }
}
