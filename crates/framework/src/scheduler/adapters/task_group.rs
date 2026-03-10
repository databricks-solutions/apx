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
}
