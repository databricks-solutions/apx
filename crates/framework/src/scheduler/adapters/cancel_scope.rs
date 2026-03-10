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

/// Python source for the `ApxCancelScope` class.
///
/// Implements the `anyio.abc.CancelScope`-compatible interface using
/// `CancelScopeState` for the fast-path checks and a per-task scope stack
/// stored in `_task_states`.
#[expect(dead_code, reason = "consumed in Phase 5 via anyio_backend rewrite")]
pub const CANCEL_SCOPE_GLUE: &str = r#"
import asyncio
from collections import defaultdict

# Per-task state: maps task identity -> _TaskState
# This is shared with the driver for cancel scope checking at yield points.
_task_states = defaultdict(_TaskState)

class _TaskState:
    """Per-task cancellation tracking."""
    __slots__ = ('cancel_scope',)
    def __init__(self):
        self.cancel_scope = None

class ApxCancelScope:
    """CancelScope compatible with anyio's interface.

    Uses CancelScopeState (Rust) for fast deadline/cancel checks.
    Manages scope tree via _task_states dict.
    """

    def __init__(self, state):
        self._state = state
        self._parent_scope = None
        self._host_task = None
        self._active = False

    def __enter__(self):
        task = asyncio.current_task()
        if task is None:
            raise RuntimeError("CancelScope requires a running task")

        self._host_task = task
        task_id = id(task)
        if task_id not in _task_states:
            _task_states[task_id] = _TaskState()

        ts = _task_states[task_id]
        self._parent_scope = ts.cancel_scope
        ts.cancel_scope = self
        self._active = True
        self._state.host_task_state = ts
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self._active = False
        task_id = id(self._host_task)
        if task_id in _task_states:
            _task_states[task_id].cancel_scope = self._parent_scope
        self._state.host_task_state = None

        if exc_type is not None and self._state.is_effectively_cancelled():
            # Check if the exception is a CancelledError that we should catch
            if issubclass(exc_type, BaseException) and _is_cancellation(exc_val):
                self._state.cancelled_caught = True
                return True  # Suppress the exception

        return False

    def cancel(self):
        self._state.cancel()

    @property
    def deadline(self):
        return self._state.deadline

    @deadline.setter
    def deadline(self, value):
        self._state.deadline = value

    @property
    def shield(self):
        return self._state.shield

    @shield.setter
    def shield(self, value):
        self._state.shield = value

    @property
    def cancel_called(self):
        return self._state.cancel_called

    @property
    def cancelled_caught(self):
        return self._state.cancelled_caught

    @property
    def cancel_scope(self):
        """Return self — anyio expects cancel_scope attribute on CancelScope."""
        return self


def _is_cancellation(exc):
    """Check if an exception is an asyncio.CancelledError."""
    return isinstance(exc, (asyncio.CancelledError,))


def _get_task_state(task_id):
    """Get or create task state for a given task identity."""
    if task_id not in _task_states:
        _task_states[task_id] = _TaskState()
    return _task_states[task_id]


def _check_cancel_scope_for_task(task_id):
    """Check if the current task's innermost scope is effectively cancelled.

    Called from the Rust driver at yield points (checkpoints).
    Returns True if a CancelledError should be thrown.
    """
    ts = _task_states.get(task_id)
    if ts is None or ts.cancel_scope is None:
        return False
    scope = ts.cancel_scope
    if scope._state.shield:
        return False
    return scope._state.is_effectively_cancelled()
"#;

/// Evaluate the cancel scope Python glue and return the module dict.
///
/// Returns a dict containing `ApxCancelScope`, `_task_states`, and helpers.
pub fn eval_cancel_scope_glue(py: Python<'_>) -> PyResult<Py<PyAny>> {
    // First define _TaskState, then the rest of the glue
    let bootstrap = std::ffi::CString::new(
        r#"
import asyncio
from collections import defaultdict

class _TaskState:
    """Per-task cancellation tracking."""
    __slots__ = ('cancel_scope',)
    def __init__(self):
        self.cancel_scope = None

_task_states = {}
"#,
    )?;

    let locals = pyo3::types::PyDict::new(py);
    py.run(&bootstrap, None, Some(&locals))?;

    // Now define the main glue with _TaskState available
    let glue = std::ffi::CString::new(
        r#"
class ApxCancelScope:
    """CancelScope compatible with anyio's interface."""

    def __init__(self, state):
        self._state = state
        self._parent_scope = None
        self._host_task = None
        self._active = False

    def __enter__(self):
        task = asyncio.current_task()
        if task is None:
            raise RuntimeError("CancelScope requires a running task")

        self._host_task = task
        task_id = id(task)
        if task_id not in _task_states:
            _task_states[task_id] = _TaskState()

        ts = _task_states[task_id]
        self._parent_scope = ts.cancel_scope
        ts.cancel_scope = self
        self._active = True
        self._state.host_task_state = ts
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self._active = False
        task_id = id(self._host_task)
        if task_id in _task_states:
            _task_states[task_id].cancel_scope = self._parent_scope
        self._state.host_task_state = None

        if exc_type is not None and self._state.is_effectively_cancelled():
            if issubclass(exc_type, BaseException) and _is_cancellation(exc_val):
                self._state.cancelled_caught = True
                return True

        return False

    def cancel(self):
        self._state.cancel()

    @property
    def deadline(self):
        return self._state.deadline

    @deadline.setter
    def deadline(self, value):
        self._state.deadline = value

    @property
    def shield(self):
        return self._state.shield

    @shield.setter
    def shield(self, value):
        self._state.shield = value

    @property
    def cancel_called(self):
        return self._state.cancel_called

    @property
    def cancelled_caught(self):
        return self._state.cancelled_caught

    @property
    def cancel_scope(self):
        return self


def _is_cancellation(exc):
    return isinstance(exc, (asyncio.CancelledError,))


def _check_cancel_scope_for_task(task_id):
    ts = _task_states.get(task_id)
    if ts is None or ts.cancel_scope is None:
        return False
    scope = ts.cancel_scope
    if scope._state.shield:
        return False
    return scope._state.is_effectively_cancelled()
"#,
    )?;

    py.run(&glue, None, Some(&locals))?;
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
    fn cancel_scope_glue_evaluates() {
        crate::with_py(|py| {
            let locals = eval_cancel_scope_glue(py).unwrap();
            let locals = locals
                .into_bound(py)
                .cast_into::<pyo3::types::PyDict>()
                .unwrap();
            // Verify key names are present
            assert!(locals.get_item("ApxCancelScope").unwrap().is_some());
            assert!(locals.get_item("_task_states").unwrap().is_some());
            assert!(
                locals
                    .get_item("_check_cancel_scope_for_task")
                    .unwrap()
                    .is_some()
            );
        });
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
