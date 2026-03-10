//! Pure-Rust scheduling primitives for the persistent event loop.
//!
//! `CoroutineScheduler` and `TaskCallback` implement asyncio's
//! `call_soon_threadsafe` + `Task.add_done_callback` scheduling pattern
//! as Rust pyclasses — no Python source code strings.

use crate::error::AppError;
use pyo3::prelude::*;
use tokio::sync::oneshot;

/// Scheduled on the event loop via `call_soon_threadsafe`.
///
/// When called (with no arguments, by the event loop), creates an
/// `asyncio.Task` from the coroutine and attaches the done callback.
/// Uses a cached `loop.create_task` reference — no `asyncio` module dispatch.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct CoroutineScheduler {
    coro: Option<Py<PyAny>>,
    callback: Option<Py<PyAny>>,
    create_task: Option<Py<PyAny>>,
}

impl CoroutineScheduler {
    pub fn new(coro: Py<PyAny>, callback: Py<PyAny>, create_task: Py<PyAny>) -> Self {
        Self {
            coro: Some(coro),
            callback: Some(callback),
            create_task: Some(create_task),
        }
    }
}

impl std::fmt::Debug for CoroutineScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoroutineScheduler")
            .field("pending", &self.coro.is_some())
            .finish()
    }
}

#[pymethods]
impl CoroutineScheduler {
    fn __call__(&mut self, py: Python<'_>) -> PyResult<()> {
        let coro = self.coro.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("scheduler already consumed")
        })?;
        let callback = self.callback.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("scheduler already consumed")
        })?;
        let create_task = self.create_task.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("scheduler already consumed")
        })?;
        let task = create_task.call1(py, (coro,))?;
        task.call_method1(py, c"add_done_callback", (callback,))?;
        Ok(())
    }
}

/// Receives asyncio Task completion via `add_done_callback`.
///
/// Extracts `task.result()` or catches the exception, classifies it,
/// and sends the result through a Tokio oneshot channel.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct TaskCallback {
    tx: Option<oneshot::Sender<Result<Py<PyAny>, AppError>>>,
}

impl TaskCallback {
    pub fn new(tx: oneshot::Sender<Result<Py<PyAny>, AppError>>) -> Self {
        Self { tx: Some(tx) }
    }
}

impl std::fmt::Debug for TaskCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCallback")
            .field("pending", &self.tx.is_some())
            .finish()
    }
}

#[pymethods]
impl TaskCallback {
    fn __call__(&mut self, py: Python<'_>, task: &Bound<'_, PyAny>) -> PyResult<()> {
        let Some(tx) = self.tx.take() else {
            return Ok(());
        };

        if task.call_method0(c"cancelled")?.is_truthy()? {
            let _ = tx.send(Err(AppError::Internal("task cancelled".to_owned())));
            return Ok(());
        }

        match task.call_method0(c"result") {
            Ok(result) => {
                let _ = tx.send(Ok(result.unbind()));
            }
            Err(e) => {
                let _ = tx.send(Err(classify_python_error(py, &e)));
            }
        }
        Ok(())
    }
}

/// Convert a Python exception to `AppError::Internal`.
///
/// User-facing HTTP errors (404, 400, etc.) are handled by FastAPI's
/// exception middleware via the ASGI bridge. Only infrastructure errors
/// reach this point.
fn classify_python_error(_py: Python<'_>, err: &PyErr) -> AppError {
    AppError::Internal(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::with_py;

    #[test]
    fn coroutine_scheduler_debug() {
        with_py(|py| {
            let coro = py.None();
            let callback = py.None();
            let create_task = py.None();
            let scheduler = CoroutineScheduler::new(coro, callback, create_task);
            let dbg = format!("{scheduler:?}");
            assert!(dbg.contains("CoroutineScheduler"));
            assert!(dbg.contains("pending: true"));
        });
    }

    #[test]
    fn task_callback_debug() {
        let (tx, _rx) = oneshot::channel();
        let cb = TaskCallback::new(tx);
        let dbg = format!("{cb:?}");
        assert!(dbg.contains("TaskCallback"));
        assert!(dbg.contains("pending: true"));
    }

    #[test]
    fn task_callback_debug_after_consume() {
        let (tx, _rx) = oneshot::channel::<Result<Py<PyAny>, AppError>>();
        let mut cb = TaskCallback { tx: Some(tx) };
        cb.tx.take();
        let dbg = format!("{cb:?}");
        assert!(dbg.contains("pending: false"));
    }

    #[test]
    fn classify_python_error_returns_internal() {
        with_py(|py| {
            let err = PyErr::new::<pyo3::exceptions::PyValueError, _>("oops");
            let app_err = classify_python_error(py, &err);
            assert!(
                matches!(app_err, AppError::Internal(ref s) if s.contains("oops")),
                "expected Internal, got {app_err:?}"
            );
        });
    }
}
