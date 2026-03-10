//! Scheduling primitives for the persistent event loop.
//!
//! [`TaskCallback`] receives asyncio `Task.add_done_callback` completions
//! and sends results through a Tokio oneshot channel back to the caller.

use crate::error::AppError;
use pyo3::prelude::*;
use tokio::sync::oneshot;

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
