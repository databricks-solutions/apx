//! [`BlockingTask`] and [`spawn_blocking`] — awaitable for work on blocking threads.

use pyo3::prelude::*;
use tokio::sync::oneshot;

/// Awaitable representing work spawned on a blocking thread.
///
/// Created via [`spawn_blocking`]. Implements the Python awaitable protocol:
/// polls the internal `oneshot::Receiver` and raises `StopIteration(result)`
/// when the blocking work completes.
#[pyclass(module = "apx._core")]
pub struct BlockingTask {
    rx: Option<oneshot::Receiver<PyResult<Py<PyAny>>>>,
    result: Option<PyResult<Py<PyAny>>>,
}

impl BlockingTask {
    /// Create a `BlockingTask` wired to the given oneshot receiver.
    ///
    /// Used by adapters that spawn actual blocking work on a separate thread
    /// and need to hand back an awaitable.
    pub(crate) fn with_receiver(rx: oneshot::Receiver<PyResult<Py<PyAny>>>) -> Self {
        Self {
            rx: Some(rx),
            result: None,
        }
    }
}

impl std::fmt::Debug for BlockingTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingTask")
            .field("done", &self.result.is_some())
            .finish()
    }
}

#[pymethods]
impl BlockingTask {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let mut this = slf.borrow_mut(py);

        // Already resolved — raise immediately.
        if let Some(ref result) = this.result {
            return match result {
                Ok(value) => Err(pyo3::exceptions::PyStopIteration::new_err((
                    value.clone_ref(py),
                ))),
                Err(err) => Err(err.clone_ref(py)),
            };
        }

        // Try to receive from the oneshot channel.
        if let Some(ref mut rx) = this.rx {
            match rx.try_recv() {
                Ok(Ok(value)) => {
                    let stop = pyo3::exceptions::PyStopIteration::new_err((value.clone_ref(py),));
                    this.result = Some(Ok(value));
                    this.rx = None;
                    Err(stop)
                }
                Ok(Err(err)) => {
                    let py_err = err.clone_ref(py);
                    this.result = Some(Err(err));
                    this.rx = None;
                    Err(py_err)
                }
                Err(oneshot::error::TryRecvError::Empty) => Ok(py.None()),
                Err(oneshot::error::TryRecvError::Closed) => {
                    let err = pyo3::exceptions::PyRuntimeError::new_err(
                        "BlockingTask: sender dropped without producing a result",
                    );
                    this.result = Some(Err(err.clone_ref(py)));
                    this.rx = None;
                    Err(err)
                }
            }
        } else {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "BlockingTask: no channel and no result",
            ))
        }
    }

    /// Check whether the blocking task has completed.
    fn done(&self) -> bool {
        self.result.is_some()
    }
}

/// Spawn a Python callable on a blocking thread and return an awaitable.
///
/// NOTE: For now, this creates the structure with a oneshot channel. The
/// actual `spawn_blocking` integration (calling the callable on a blocking
/// thread with GIL acquired) happens in the driver/adapters layer.
#[pyfunction]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "registered as Python function when blocking dispatch is wired"
    )
)]
pub fn spawn_blocking(_py: Python<'_>, _callable: Py<PyAny>) -> PyResult<BlockingTask> {
    let (_tx, rx) = oneshot::channel();
    Ok(BlockingTask {
        rx: Some(rx),
        result: None,
    })
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn blocking_task_not_done_initially() {
        crate::with_py(|py| {
            let task = spawn_blocking(py, py.None()).unwrap();
            assert!(!task.done());
        });
    }
}
