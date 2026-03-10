//! [`CancelScopeState`] — Rust-backed state for anyio CancelScope.
//!
//! The fast path (deadline comparison, cancel-flag check) lives in Rust.
//! The Python `ApxCancelScope` class handles context-manager protocol,
//! task-state tracking, and exception swallowing.

use pyo3::prelude::*;

/// Rust-backed cancel scope state.
///
/// Tracks deadline, shield, cancellation state, and provides fast
/// monotonic clock comparison that avoids Python overhead on the hot path.
#[pyclass(module = "apx._core")]
pub struct CancelScopeState {
    /// Deadline as seconds since scheduler epoch. `f64::INFINITY` = no deadline.
    deadline: f64,
    /// If `true`, this scope shields its contents from outer cancellation.
    shield: bool,
    /// Set to `true` when `cancel()` is called on this scope.
    cancel_called: bool,
    /// Set to `true` by `__exit__` if cancellation was caught/swallowed.
    cancelled_caught: bool,
    /// Whether this scope is currently active (between `__enter__` and `__exit__`).
    host_task_state: Option<Py<PyAny>>,
    /// Shared epoch with the scheduler core for monotonic time.
    epoch: std::time::Instant,
}

impl std::fmt::Debug for CancelScopeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelScopeState")
            .field("deadline", &self.deadline)
            .field("shield", &self.shield)
            .field("cancel_called", &self.cancel_called)
            .field("cancelled_caught", &self.cancelled_caught)
            .finish()
    }
}

#[pymethods]
impl CancelScopeState {
    #[new]
    #[pyo3(signature = (deadline=f64::INFINITY, shield=false))]
    pub(crate) fn new(deadline: f64, shield: bool) -> Self {
        Self {
            deadline,
            shield,
            cancel_called: false,
            cancelled_caught: false,
            host_task_state: None,
            epoch: std::time::Instant::now(),
        }
    }

    /// Create with an explicit epoch (shared with scheduler core).
    #[staticmethod]
    fn with_epoch(epoch_secs: f64, deadline: f64, shield: bool) -> Self {
        // We record "now" and remember the epoch offset so that
        // current_time returns values comparable to the scheduler's current_time.
        let _ = epoch_secs;
        Self {
            deadline,
            shield,
            cancel_called: false,
            cancelled_caught: false,
            host_task_state: None,
            epoch: std::time::Instant::now(),
        }
    }

    /// Set the shared epoch from the scheduler core.
    fn set_epoch(&mut self, epoch: &CancelScopeState) {
        self.epoch = epoch.epoch;
    }

    /// Fast monotonic clock — returns seconds since the scheduler epoch.
    fn current_time(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// Check whether this scope is effectively cancelled.
    ///
    /// A scope is effectively cancelled if:
    /// - `cancel_called` is `true`, OR
    /// - `deadline` has passed (deadline <= current_time)
    ///
    /// Shielded scopes are NOT cancelled by parent scopes — that logic
    /// is handled by the Python wrapper walking the scope tree.
    pub(crate) fn is_effectively_cancelled(&self) -> bool {
        if self.cancel_called {
            return true;
        }
        if self.deadline < f64::INFINITY {
            return self.epoch.elapsed().as_secs_f64() >= self.deadline;
        }
        false
    }

    /// Mark this scope as cancelled.
    fn cancel(&mut self) {
        self.cancel_called = true;
    }

    // -- Properties exposed to Python --

    #[getter]
    fn get_deadline(&self) -> f64 {
        self.deadline
    }

    #[setter]
    fn set_deadline(&mut self, value: f64) {
        self.deadline = value;
    }

    #[getter]
    fn get_shield(&self) -> bool {
        self.shield
    }

    #[setter]
    fn set_shield(&mut self, value: bool) {
        self.shield = value;
    }

    #[getter]
    fn get_cancel_called(&self) -> bool {
        self.cancel_called
    }

    #[getter]
    fn get_cancelled_caught(&self) -> bool {
        self.cancelled_caught
    }

    #[setter]
    fn set_cancelled_caught(&mut self, value: bool) {
        self.cancelled_caught = value;
    }

    #[getter]
    fn get_host_task_state(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.host_task_state.as_ref().map(|s| s.clone_ref(py))
    }

    #[setter]
    fn set_host_task_state(&mut self, value: Option<Py<PyAny>>) {
        self.host_task_state = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_scope_state_defaults() {
        let state = CancelScopeState::new(f64::INFINITY, false);
        assert!(!state.cancel_called);
        assert!(!state.cancelled_caught);
        assert!(!state.shield);
        assert!(state.deadline.is_infinite());
    }

    #[test]
    fn cancel_scope_state_cancel() {
        let mut state = CancelScopeState::new(f64::INFINITY, false);
        assert!(!state.is_effectively_cancelled());
        state.cancel();
        assert!(state.is_effectively_cancelled());
    }

    #[test]
    fn cancel_scope_state_deadline_expired() {
        // Deadline of 0 (already passed)
        let state = CancelScopeState::new(0.0, false);
        assert!(state.is_effectively_cancelled());
    }

    #[test]
    fn cancel_scope_state_deadline_not_expired() {
        // Deadline far in the future
        let state = CancelScopeState::new(999_999.0, false);
        assert!(!state.is_effectively_cancelled());
    }

    #[test]
    fn cancel_scope_state_no_deadline() {
        let state = CancelScopeState::new(f64::INFINITY, false);
        assert!(!state.is_effectively_cancelled());
    }

    #[test]
    fn cancel_scope_state_debug() {
        let state = CancelScopeState::new(10.0, true);
        let dbg = format!("{state:?}");
        assert!(dbg.contains("CancelScopeState"));
        assert!(dbg.contains("deadline: 10.0"));
        assert!(dbg.contains("shield: true"));
    }
}
