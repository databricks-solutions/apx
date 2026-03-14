//! Coroutine driver — the core innovation of the Rust scheduler.
//!
//! Replaces `asyncio.Task._step` by calling `coro.send(value)` directly from
//! Rust and interpreting yielded values to decide what to do next.
//!
//! The most common case (36% of all primitive calls) is `yield None` — the
//! driver handles this by looping immediately without any
//! Python→asyncio→Python round trip.

use std::sync::Arc;

use pyo3::PyTypeInfo;
use pyo3::prelude::*;
use pyo3::types::PyType;
use tokio::sync::oneshot;

use super::primitives::{EventWaiter, Future};
use super::queue::{ReadyQueue, ReadyTask};
use super::task::{SchedulerTask, TaskProxy};
use crate::error::AppError;

// ---------------------------------------------------------------------------
// CachedTypes — pre-resolved Python type references
// ---------------------------------------------------------------------------

/// Pre-resolved Python type references, cached at startup to avoid repeated
/// `import` / `getattr` calls in the hot path.
pub struct CachedTypes {
    /// `types.CoroutineType` — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    coroutine_type: Py<PyType>,
    /// Our `Future` type object — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    future_type: Py<PyType>,
    /// Our `EventWaiter` type object — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    event_waiter_type: Py<PyType>,
    // -- Raw type pointers for hot-path ob_type comparison ------------------
    //
    // Borrowed from the Py<PyType> fields above — valid as long as CachedTypes
    // lives (the Py handles prevent the type objects from being deallocated).
    /// Raw `ob_type` pointer for our `Future` pyclass.
    future_type_ptr: *mut pyo3::ffi::PyObject,
    /// Raw `ob_type` pointer for our `EventWaiter` pyclass.
    event_waiter_type_ptr: *mut pyo3::ffi::PyObject,
    /// Raw `ob_type` pointer for `types.CoroutineType`.
    coroutine_type_ptr: *mut pyo3::ffi::PyObject,
    /// Interned `"_asyncio_future_blocking"` attribute name for duck-type
    /// asyncio.Future detection via `_asyncio_future_blocking` attribute.
    asyncio_future_blocking_attr: Py<PyAny>,
    /// Cached `Py_None` pointer for yield-None fast path.
    py_none_ptr: *mut pyo3::ffi::PyObject,
}

// Safety: The raw pointers in CachedTypes are borrowed from Py<PyType> handles
// that are kept alive for the struct's entire lifetime. The pointers are only
// dereferenced under the GIL (which Python<'_> proves), so they are safe to
// send across threads.
#[expect(
    unsafe_code,
    reason = "raw pointers borrowed from GIL-protected Py handles"
)]
unsafe impl Send for CachedTypes {}
#[expect(
    unsafe_code,
    reason = "raw pointers borrowed from GIL-protected Py handles"
)]
unsafe impl Sync for CachedTypes {}

impl std::fmt::Debug for CachedTypes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedTypes").finish_non_exhaustive()
    }
}

impl CachedTypes {
    /// Resolve all Python types once at startup.
    pub fn resolve(py: Python<'_>) -> PyResult<Self> {
        let types = py.import(c"types")?;

        let future_type = Future::type_object(py).unbind();
        let event_waiter_type = EventWaiter::type_object(py).unbind();
        let coroutine_type = types
            .getattr(c"CoroutineType")?
            .cast_into::<PyType>()?
            .unbind();

        // Cache raw type pointers for hot-path ob_type comparison.
        let future_type_ptr = future_type.as_ptr();
        let event_waiter_type_ptr = event_waiter_type.as_ptr();
        let coroutine_type_ptr = coroutine_type.as_ptr();
        let py_none_ptr = py.None().as_ptr();

        // Intern "_asyncio_future_blocking" for duck-type asyncio.Future detection.
        let asyncio_future_blocking_attr: Py<PyAny> = pyo3::intern!(py, "_asyncio_future_blocking")
            .clone()
            .unbind()
            .into();

        Ok(Self {
            coroutine_type,
            future_type,
            event_waiter_type,
            future_type_ptr,
            event_waiter_type_ptr,
            coroutine_type_ptr,
            asyncio_future_blocking_attr,
            py_none_ptr,
        })
    }
}

// ---------------------------------------------------------------------------
// StepResult — outcome of advancing a coroutine one step
// ---------------------------------------------------------------------------

/// Outcome of a single `send` / `throw` cycle on a coroutine.
#[derive(Debug)]
pub enum StepResult {
    /// Coroutine yielded a value (needs classification).
    Yielded(Py<PyAny>),
    /// Coroutine completed (raised `StopIteration` with a value).
    Completed(Py<PyAny>),
    /// Coroutine raised an exception.
    Error(PyErr),
}

// ---------------------------------------------------------------------------
// step / step_throw — single send/throw cycle
// ---------------------------------------------------------------------------

/// Advance a coroutine one step via C-level `PyIter_Send`.
///
/// # Safety invariants (upheld by callers)
/// - `coro` is a valid Python coroutine/iterator
/// - `py` proves the GIL is held
///
/// `PyIter_Send` handles `StopIteration` internally — no Python exception on
/// `PYGEN_RETURN`.
#[expect(
    unsafe_code,
    reason = "FFI call to PyIter_Send for hot-path performance"
)]
pub fn step(py: Python<'_>, coro: &Py<PyAny>, value: Option<&Py<PyAny>>) -> StepResult {
    let send_arg = value.map_or(std::ptr::null_mut(), |v| v.as_ptr());
    let mut result_ptr: *mut pyo3::ffi::PyObject = std::ptr::null_mut();

    // Safety: coro is a valid Python coroutine, py proves GIL is held.
    // PyIter_Send handles StopIteration internally — no Python exception on PYGEN_RETURN.
    let status = unsafe { pyo3::ffi::PyIter_Send(coro.as_ptr(), send_arg, &raw mut result_ptr) };

    match status {
        pyo3::ffi::PySendResult::PYGEN_NEXT => {
            // Safety: PYGEN_NEXT guarantees result_ptr is a new reference.
            let obj = unsafe { Bound::from_owned_ptr(py, result_ptr) }.unbind();
            StepResult::Yielded(obj)
        }
        pyo3::ffi::PySendResult::PYGEN_RETURN => {
            let value = if result_ptr.is_null() {
                py.None()
            } else {
                // Safety: PYGEN_RETURN with non-null is a new reference to the return value.
                unsafe { Bound::from_owned_ptr(py, result_ptr) }.unbind()
            };
            StepResult::Completed(value)
        }
        pyo3::ffi::PySendResult::PYGEN_ERROR => {
            // PYGEN_ERROR means a Python exception should be set.
            // Guard against edge cases where no exception is actually set
            // (e.g. generator already exhausted).
            if unsafe { !pyo3::ffi::PyErr_Occurred().is_null() } {
                let err = PyErr::fetch(py);
                // Check if it's a StopIteration — treat as completion.
                if err.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
                    let value = err
                        .value(py)
                        .getattr(c"value")
                        .map_or_else(|_| py.None(), |v| v.unbind());
                    StepResult::Completed(value)
                } else {
                    StepResult::Error(err)
                }
            } else {
                // No exception set — treat as completion with None.
                StepResult::Completed(py.None())
            }
        }
    }
}

/// Advance a coroutine one step by calling `coro.throw(exc)`.
///
/// Python 3.12+ accepts just the exception instance. No C-level equivalent
/// of `throw()` with the same simplicity — `throw` is the cold path (only
/// on exceptions), so the overhead is negligible.
pub fn step_throw(py: Python<'_>, coro: &Py<PyAny>, err: PyErr) -> StepResult {
    match coro.call_method1(py, c"throw", (err.value(py),)) {
        Ok(yielded) => StepResult::Yielded(yielded),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) => {
            let value = e
                .value(py)
                .getattr(c"value")
                .map_or_else(|_| py.None(), |v| v.unbind());
            StepResult::Completed(value)
        }
        Err(e) => StepResult::Error(e),
    }
}

// ---------------------------------------------------------------------------
// AwaitableKind — classification of yielded objects
// ---------------------------------------------------------------------------

/// Classification of a yielded object from a coroutine.
pub enum AwaitableKind {
    /// Our own `Future` — poll directly via `__next__`.
    Future,
    /// Our own `EventWaiter` — check event flag.
    EventWaiter,
    /// `asyncio.Future` (not Task) — attach done callback.
    AsyncioFuture,
    /// A coroutine object — push onto coroutine stack.
    Coroutine,
    /// `yield None` — reschedule immediately (like `call_soon`).
    YieldNone,
    /// Unknown awaitable — unsupported, returns error.
    Unknown,
}

// ---------------------------------------------------------------------------
// classify — inspects a yielded object
// ---------------------------------------------------------------------------

/// Classify a yielded value to determine what the driver should do next.
///
/// Check order is optimised for the common case: `yield None` first (36% of
/// all calls), then our own types, then coroutines, then asyncio futures.
///
/// Uses direct `ob_type` pointer comparisons for our own pyclass types (no
/// subclassing) and `Py_None` singleton comparison. Falls back to duck-type
/// attribute check for asyncio futures (`_asyncio_future_blocking`).
#[expect(
    unsafe_code,
    reason = "ob_type pointer read under GIL for hot-path classification"
)]
pub fn classify(_py: Python<'_>, yielded: &Py<PyAny>, types: &CachedTypes) -> AwaitableKind {
    let obj_ptr = yielded.as_ptr();

    // Fast path: yield None (36% of all calls) — pointer comparison against singleton.
    if obj_ptr == types.py_none_ptr {
        return AwaitableKind::YieldNone;
    }

    // Safety: ob_type is always valid for a live Python object while GIL is held.
    let ob_type = unsafe { (*obj_ptr).ob_type.cast::<pyo3::ffi::PyObject>() };

    // Exact type match for our own pyclass types (no subclassing allowed).
    if ob_type == types.future_type_ptr {
        return AwaitableKind::Future;
    }
    if ob_type == types.event_waiter_type_ptr {
        return AwaitableKind::EventWaiter;
    }

    // Coroutine: exact type match (native coroutines have a fixed type).
    if ob_type == types.coroutine_type_ptr {
        return AwaitableKind::Coroutine;
    }

    // asyncio.Future detection via _asyncio_future_blocking attribute.
    // Duck-type asyncio.Future detection — checking for the
    // attribute is faster than isinstance on the full MRO.
    let blocking_attr = unsafe {
        pyo3::ffi::PyObject_GetAttr(obj_ptr, types.asyncio_future_blocking_attr.as_ptr())
    };
    if !blocking_attr.is_null() {
        unsafe { pyo3::ffi::Py_DECREF(blocking_attr) };
        return AwaitableKind::AsyncioFuture;
    }
    // Clear the AttributeError from the failed GetAttr.
    unsafe { pyo3::ffi::PyErr_Clear() };

    AwaitableKind::Unknown
}

// ---------------------------------------------------------------------------
// DriveResult — outcome of driving a task
// ---------------------------------------------------------------------------

/// Outcome of driving a [`SchedulerTask`] until it suspends or completes.
pub enum DriveResult {
    /// Task completed with a value.
    Completed(Py<PyAny>),
    /// Task raised an exception.
    Error(PyErr),
    /// Waiting on a `Future` — resume when it resolves.
    WaitingOnFuture(Py<PyAny>),
    /// Waiting on a `EventWaiter` — resume when event is set.
    WaitingOnEvent(Py<PyAny>),
    /// Waiting on an `asyncio.Future` — attach done callback.
    WaitingOnAsyncioFuture(Py<PyAny>),
    /// Step budget exhausted — re-enqueue for fairness.
    BudgetExhausted,
}

/// Maximum `YieldNone` steps before re-enqueueing for fairness.
const DEFAULT_STEP_BUDGET: usize = 128;

// ---------------------------------------------------------------------------
// drive_task — the main drive loop
// ---------------------------------------------------------------------------

/// Drive a [`SchedulerTask`] until it suspends or completes.
///
/// This is the hot loop. When the yielded object is `None` (most common case),
/// it immediately loops without suspending. Sub-coroutines are pushed onto the
/// task's coroutine stack and driven inline.
#[expect(
    clippy::needless_continue,
    reason = "explicit `continue` documents intent to re-enter the drive loop"
)]
pub fn drive_task(
    py: Python<'_>,
    task: &mut SchedulerTask,
    types: &CachedTypes,
    step_budget: usize,
) -> DriveResult {
    let mut steps: usize = 0;
    loop {
        // Clone the coro ref so the immutable borrow on `task` is released
        // before we mutate it (take_throw_error / take_send_value).
        let coro = match task.active_coro() {
            Ok(c) => c.clone_ref(py),
            Err(e) => return DriveResult::Error(e),
        };
        let step_result = if let Some(err) = task.take_throw_error() {
            step_throw(py, &coro, err)
        } else {
            let send_val = task.take_send_value();
            step(py, &coro, send_val.as_ref())
        };

        match step_result {
            StepResult::Yielded(obj) => {
                let kind = classify(py, &obj, types);
                match kind {
                    AwaitableKind::YieldNone => {
                        steps += 1;
                        if steps >= step_budget {
                            return DriveResult::BudgetExhausted;
                        }
                        continue;
                    }
                    AwaitableKind::Future => {
                        return DriveResult::WaitingOnFuture(obj);
                    }
                    AwaitableKind::EventWaiter => {
                        return DriveResult::WaitingOnEvent(obj);
                    }
                    AwaitableKind::Coroutine => {
                        task.push_coro(obj);
                        continue; // drive the sub-coroutine
                    }
                    AwaitableKind::AsyncioFuture => {
                        return DriveResult::WaitingOnAsyncioFuture(obj);
                    }
                    AwaitableKind::Unknown => {
                        let type_name = obj
                            .bind(py)
                            .get_type()
                            .name()
                            .map_or_else(|_| "<unknown>".to_owned(), |n| n.to_string());
                        return DriveResult::Error(pyo3::exceptions::PyTypeError::new_err(
                            format!("unsupported awaitable type yielded: {type_name}"),
                        ));
                    }
                }
            }
            StepResult::Completed(value) => {
                if task.pop_coro() {
                    // Sub-coroutine completed — send its result to parent.
                    task.set_send_value(value);
                    continue;
                }
                // Top-level coroutine completed.
                return DriveResult::Completed(value);
            }
            StepResult::Error(e) => {
                if task.pop_coro() {
                    // Sub-coroutine raised — throw into parent.
                    task.set_throw_error(e);
                    continue;
                }
                return DriveResult::Error(e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ResumeCallback — re-drives the task when a future resolves
// ---------------------------------------------------------------------------

/// Callback that re-drives a [`SchedulerTask`] after a suspended awaitable
/// resolves.
///
/// Used as `add_done_callback` on asyncio/Rust futures, and as a
/// `call_soon` target for event waiter re-polls. The optional `future`
/// argument distinguishes the two cases: present for done callbacks, absent
/// for re-poll.
#[pyclass(module = "apx._core")]
pub struct ResumeCallback {
    task: Option<SchedulerTask>,
    proxy: Option<Py<TaskProxy>>,
    queue: Arc<ReadyQueue>,
}

impl std::fmt::Debug for ResumeCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeCallback")
            .field("has_task", &self.task.is_some())
            .finish()
    }
}

#[pymethods]
impl ResumeCallback {
    /// Called by Python when the awaited future completes, or by `call_soon`
    /// for event waiter re-polls.
    ///
    /// O(1): extracts the future result and enqueues the task for the drain
    /// loop. No `drive_task` here — all re-drive work happens in the drain.
    #[pyo3(signature = (future=None))]
    fn __call__(&mut self, py: Python<'_>, future: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let mut task = self.task.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("ResumeCallback invoked twice")
        })?;

        if let Some(fut) = future {
            match extract_future_result(py, fut) {
                Ok(value) => task.set_send_value(value),
                Err(err) => task.set_throw_error(err),
            }
        }

        let proxy = self.proxy.take();
        tracing::trace!("resume_callback: enqueue");
        self.queue.push(py, ReadyTask { task, proxy });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// extract_future_result — pull result from a resolved future
// ---------------------------------------------------------------------------

/// Extract the result from a resolved Python future.
///
/// Handles both `asyncio.Future` (checks `cancelled()` first) and
/// `Future` (which has no `cancelled` method — the attribute error
/// is silently ignored).
fn extract_future_result(py: Python<'_>, future: &Bound<'_, PyAny>) -> Result<Py<PyAny>, PyErr> {
    if let Ok(cancelled) = future.call_method0(c"cancelled")
        && cancelled.is_truthy().unwrap_or(false)
    {
        let cls = py.import(c"asyncio")?.getattr(c"CancelledError")?;
        return Err(PyErr::from_value(cls.call0()?));
    }
    future.call_method0(c"result").map(|v| v.unbind())
}

// ---------------------------------------------------------------------------
// handle_drive_result — dispatch on DriveResult after driving
// ---------------------------------------------------------------------------

/// Route a [`DriveResult`] to the appropriate continuation.
///
/// Either completes the task (sending through `result_tx`), or creates a
/// [`ResumeCallback`] and attaches it to the awaitable so the task is
/// re-driven when the awaitable resolves.
fn handle_drive_result(
    py: Python<'_>,
    mut task: SchedulerTask,
    drive_result: DriveResult,
    call_soon: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<()> {
    match drive_result {
        DriveResult::Completed(value) => {
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Ok(value));
            }
            Ok(())
        }
        DriveResult::Error(err) => {
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Err(AppError::Internal(err.to_string())));
            }
            Ok(())
        }
        DriveResult::WaitingOnFuture(fut) => handle_rust_future(py, task, fut, ready_queue, proxy),
        DriveResult::WaitingOnEvent(_waiter) => {
            let cb = make_resume_callback(py, task, ready_queue, proxy)?;
            call_soon.call1(py, (cb,))?;
            Ok(())
        }
        DriveResult::WaitingOnAsyncioFuture(fut) => {
            let cb = make_resume_callback(py, task, ready_queue, proxy)?;
            fut.call_method1(py, c"add_done_callback", (cb,))?;
            Ok(())
        }
        DriveResult::BudgetExhausted => {
            ready_queue.push(py, ReadyTask { task, proxy });
            Ok(())
        }
    }
}

/// Handle `WaitingOnFuture`: if already done, enqueue for re-drive;
/// otherwise attach a done callback.
fn handle_rust_future(
    py: Python<'_>,
    task: SchedulerTask,
    fut: Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<()> {
    let is_done = fut.call_method0(py, c"done")?.is_truthy(py)?;
    if is_done {
        let mut task = task;
        match extract_future_result(py, fut.bind(py)) {
            Ok(value) => task.set_send_value(value),
            Err(err) => task.set_throw_error(err),
        }
        ready_queue.push(py, ReadyTask { task, proxy });
        return Ok(());
    }
    let cb = make_resume_callback(py, task, ready_queue, proxy)?;
    fut.call_method1(py, c"add_done_callback", (cb,))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// resume_task — re-drive a task from the ready queue
// ---------------------------------------------------------------------------

/// Re-drive a task that became ready via the queue.
///
/// Reinstalls the proxy, calls [`drive_task`], dispatches the result.
/// `result_tx` is inside `task` — no separate channel parameter.
pub fn resume_task(
    py: Python<'_>,
    ready: ReadyTask,
    cached_types: &Arc<CachedTypes>,
    call_soon: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
) -> PyResult<()> {
    let ReadyTask { mut task, proxy } = ready;

    let saved = install_proxy(py, proxy.as_ref());

    let drive_result = drive_task(py, &mut task, cached_types, DEFAULT_STEP_BUDGET);
    let result = handle_drive_result(py, task, drive_result, call_soon, ready_queue, proxy);

    restore_proxy(py, saved);
    result
}

// ---------------------------------------------------------------------------
// make_resume_callback — factory for ResumeCallback instances
// ---------------------------------------------------------------------------

fn make_resume_callback(
    py: Python<'_>,
    task: SchedulerTask,
    ready_queue: &Arc<ReadyQueue>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<Py<ResumeCallback>> {
    Py::new(
        py,
        ResumeCallback {
            task: Some(task),
            proxy,
            queue: Arc::clone(ready_queue),
        },
    )
}

// ---------------------------------------------------------------------------
// spawn_and_drive — entry point for the scheduler
// ---------------------------------------------------------------------------

/// Create a [`SchedulerTask`] from a coroutine and drive it.
///
/// Synchronous: either completes immediately (for trivial coroutines) or
/// suspends by attaching a [`ResumeCallback`] to the first awaitable.
/// The final result is sent through `result_tx`.
///
/// Sets the task as `asyncio.current_task()` for the duration of driving
/// so that Starlette/FastAPI middleware can create weak references to it.
pub fn spawn_and_drive(
    py: Python<'_>,
    coro: Py<PyAny>,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    cached_types: &Arc<CachedTypes>,
    call_soon: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
) {
    let mut task = match SchedulerTask::new(py, coro, result_tx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler task creation failed");
            return;
        }
    };

    // Set our task as asyncio's "current task" so Starlette/FastAPI
    // middleware that calls asyncio.current_task() gets a valid object
    // (needed for weakref support in ServerErrorMiddleware, etc.).
    let task_ctx = set_current_task(py, &task);

    let drive_result = drive_task(py, &mut task, cached_types, DEFAULT_STEP_BUDGET);
    let proxy = task_ctx.as_ref().map(|(_, p)| p.clone_ref(py));
    // Keep a reference for ownership check in clear_current_task.
    let proxy_for_clear = proxy.as_ref().map(|p| p.clone_ref(py));
    if let Err(e) = handle_drive_result(py, task, drive_result, call_soon, ready_queue, proxy) {
        tracing::warn!(error = %e, "scheduler drive result handling failed");
    }

    clear_current_task(py, task_ctx.map(|(ct, _)| ct), proxy_for_clear.as_ref());
}

/// Install a [`TaskProxy`] as `asyncio.current_task()` for the running loop.
///
/// Returns `(current_tasks_dict, proxy)` for cleanup and for storing in
/// `ResumeCallback` so that resumed coroutines also see a valid current task.
fn set_current_task(py: Python<'_>, task: &SchedulerTask) -> Option<(Py<PyAny>, Py<TaskProxy>)> {
    let asyncio = py.import(c"asyncio").ok()?;
    let tasks_mod = py.import(c"asyncio.tasks").ok()?;
    let current_tasks = tasks_mod.getattr(c"_current_tasks").ok()?;
    let loop_obj = asyncio.call_method0(c"get_running_loop").ok()?;

    let ctx = task.ctx.as_ref().map(|c| c.clone_ref(py));
    let proxy = Py::new(
        py,
        TaskProxy::new(
            task.result_future.clone_ref(py),
            loop_obj.clone().unbind(),
            task.root_coro(py),
            ctx,
        ),
    )
    .ok()?;

    let _ = current_tasks.call_method1(c"__setitem__", (&loop_obj, &proxy));
    Some((current_tasks.unbind(), proxy))
}

/// Reinstall an existing [`TaskProxy`] in `asyncio.tasks._current_tasks`.
///
/// Used by [`resume_task`] to restore the current task when re-driving.
/// Saves the previous entry so it can be restored after driving.
///
/// Returns `(current_tasks_dict, loop_obj, previous_entry)` for restoration.
fn install_proxy(
    py: Python<'_>,
    proxy: Option<&Py<TaskProxy>>,
) -> Option<(Py<PyAny>, Py<PyAny>, Py<PyAny>)> {
    let proxy = proxy?;
    let tasks_mod = py.import(c"asyncio.tasks").ok()?;
    let current_tasks = tasks_mod.getattr(c"_current_tasks").ok()?;
    let asyncio = py.import(c"asyncio").ok()?;
    let loop_obj = asyncio.call_method0(c"get_running_loop").ok()?;
    // Save previous entry before overwriting.
    let prev = current_tasks
        .call_method1(c"get", (&loop_obj, py.None()))
        .ok()?
        .unbind();
    let _ = current_tasks.call_method1(c"__setitem__", (&loop_obj, proxy));
    Some((current_tasks.unbind(), loop_obj.unbind(), prev))
}

/// Restore the previous `_current_tasks` entry after [`resume_task`].
///
/// If the previous entry was `None`, removes the dict entry.
/// This preserves any entry set by a concurrent blocking thread.
fn restore_proxy(py: Python<'_>, saved: Option<(Py<PyAny>, Py<PyAny>, Py<PyAny>)>) {
    let Some((ct, loop_obj, prev)) = saved else {
        return;
    };
    if prev.bind(py).is_none() {
        let _ = ct.call_method1(py, c"pop", (&loop_obj, py.None()));
    } else {
        let _ = ct.call_method1(py, c"__setitem__", (&loop_obj, &prev));
    }
}

/// Remove our task from `asyncio._current_tasks`, but ONLY if it still
/// matches our proxy.
fn clear_current_task(
    py: Python<'_>,
    current_tasks: Option<Py<PyAny>>,
    our_proxy: Option<&Py<TaskProxy>>,
) {
    let Some(ct) = current_tasks else { return };
    let Ok(asyncio) = py.import(c"asyncio") else {
        return;
    };
    let Ok(loop_obj) = asyncio.call_method0(c"get_running_loop") else {
        return;
    };
    // Only clear the shared dict if the entry is still our proxy.
    match our_proxy {
        Some(proxy) => {
            if ct
                .call_method1(py, c"get", (&loop_obj,))
                .is_ok_and(|current| current.bind(py).is(proxy.bind(py)))
            {
                let _ = ct.call_method1(py, c"pop", (&loop_obj, py.None()));
            }
        }
        None => {
            let _ = ct.call_method1(py, c"pop", (&loop_obj, py.None()));
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

    #[test]
    fn cached_types_resolve() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            // Verify each type object is non-null and callable.
            assert!(!types.coroutine_type.bind(py).is_none());
            assert!(!types.future_type.bind(py).is_none());
            assert!(!types.event_waiter_type.bind(py).is_none());
        });
    }

    #[test]
    fn classify_none_is_yield_none() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            let none = py.None();
            assert!(matches!(
                classify(py, &none, &types),
                AwaitableKind::YieldNone
            ));
        });
    }

    #[test]
    fn classify_rust_future() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            let future = Future::resolved(py.None());
            let py_future = Py::new(py, future).unwrap().into_any();
            assert!(matches!(
                classify(py, &py_future, &types),
                AwaitableKind::Future
            ));
        });
    }

    #[test]
    fn classify_coroutine() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            py.run(c"async def _c(): pass", None, None).unwrap();
            let coro = py.eval(c"_c()", None, None).unwrap().unbind();
            assert!(matches!(
                classify(py, &coro, &types),
                AwaitableKind::Coroutine
            ));
        });
    }

    #[test]
    fn classify_asyncio_future() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            // Create a real asyncio.Future (requires a running loop).
            let asyncio = py.import(c"asyncio").unwrap();
            let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
            let fut = loop_obj.call_method0(c"create_future").unwrap();
            let fut_py = fut.unbind();
            assert!(
                matches!(classify(py, &fut_py, &types), AwaitableKind::AsyncioFuture),
                "asyncio.Future should be classified as AsyncioFuture"
            );
            let _ = loop_obj.call_method0(c"close");
        });
    }

    #[test]
    fn step_completed_coroutine() {
        crate::with_py(|py| {
            // Create a coroutine that immediately returns 42.
            py.run(
                c"
async def coro():
    return 42
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"coro()", None, None).unwrap().unbind();
            let result = step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let num: i64 = val.extract(py).unwrap();
                assert_eq!(num, 42);
            }
        });
    }

    #[test]
    fn step_throw_propagates() {
        crate::with_py(|py| {
            py.run(
                c"
async def coro():
    return 42
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"coro()", None, None).unwrap().unbind();
            let err = pyo3::exceptions::PyValueError::new_err("test error");
            let result = step_throw(py, &coro, err);
            assert!(matches!(result, StepResult::Error(_)));
        });
    }

    #[test]
    fn step_yielding_coroutine() {
        crate::with_py(|py| {
            // Coroutine that yields None once, then returns "done".
            py.run(
                c"
import asyncio
async def yielding():
    await asyncio.sleep(0)  # yields None
    return 'done'
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"yielding()", None, None).unwrap().unbind();
            // First step: should yield (asyncio.sleep yields a future, but
            // the inner coroutine yields None).
            let result = step(py, &coro, None);
            assert!(
                matches!(result, StepResult::Yielded(_)),
                "expected Yielded, got {result:?}",
            );
            // Second step: send None back, should complete.
            let result = step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let s: String = val.extract(py).unwrap();
                assert_eq!(s, "done");
            }
        });
    }

    #[test]
    fn step_sub_coroutine() {
        crate::with_py(|py| {
            // Inner coroutine yields (via asyncio.sleep(0)), then returns.
            // outer awaits inner, so the first step yields through the chain.
            py.run(
                c"
import asyncio
async def inner():
    await asyncio.sleep(0)
    return 99

async def outer():
    val = await inner()
    return val + 1
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"outer()", None, None).unwrap().unbind();
            // First step: inner yields (via sleep(0)), propagated to outer.
            let result = step(py, &coro, None);
            assert!(
                matches!(result, StepResult::Yielded(_)),
                "expected Yielded, got {result:?}",
            );
            // Second step: send None back, inner completes, outer completes.
            let result = step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let num: i64 = val.extract(py).unwrap();
                assert_eq!(num, 100);
            }
        });
    }

    // -- spawn_and_drive tests -----------------------------------------------

    /// Helper: create a dummy `call_soon` ref for tests where it won't
    /// actually be called (trivial coroutines).
    fn dummy_call_soon(py: Python<'_>) -> Py<PyAny> {
        py.eval(c"lambda *a, **kw: None", None, None)
            .unwrap()
            .unbind()
    }

    #[test]
    fn spawn_and_drive_trivial_coroutine() {
        crate::with_py(|py| {
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let call_soon = dummy_call_soon(py);
            let ready_queue = Arc::new(ReadyQueue::new());

            py.run(c"async def _f(): return 42", None, None).unwrap();
            let coro = py.eval(c"_f()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            spawn_and_drive(py, coro, tx, &types, &call_soon, &ready_queue);

            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn spawn_and_drive_coroutine_error() {
        crate::with_py(|py| {
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let call_soon = dummy_call_soon(py);
            let ready_queue = Arc::new(ReadyQueue::new());

            py.run(c"async def _err(): raise ValueError('boom')", None, None)
                .unwrap();
            let coro = py.eval(c"_err()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            spawn_and_drive(py, coro, tx, &types, &call_soon, &ready_queue);

            let result = rx.try_recv().unwrap();
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(
                matches!(err, AppError::Internal(ref s) if s.contains("boom")),
                "expected Internal('boom'), got {err:?}"
            );
        });
    }

    #[test]
    fn resume_callback_debug() {
        crate::with_py(|py| {
            let ready_queue = Arc::new(ReadyQueue::new());
            let (tx, _rx) = oneshot::channel();

            let task = SchedulerTask::new(py, py.None(), tx).unwrap();
            let cb = ResumeCallback {
                task: Some(task),
                proxy: None,
                queue: ready_queue,
            };
            let dbg = format!("{cb:?}");
            assert!(dbg.contains("ResumeCallback"));
            assert!(dbg.contains("has_task: true"));
        });
    }

    #[test]
    fn test_contextvars_propagation() {
        crate::with_py(|py| {
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let call_soon = dummy_call_soon(py);
            let ready_queue = Arc::new(ReadyQueue::new());

            // Set a contextvar before spawn_and_drive, verify it's visible
            // inside the coroutine.
            py.run(
                c"
import contextvars
test_var = contextvars.ContextVar('test_var', default='unset')
test_var.set('hello_from_middleware')

async def _check_ctx():
    return test_var.get()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"_check_ctx()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            spawn_and_drive(py, coro, tx, &types, &call_soon, &ready_queue);

            let result = rx.try_recv().unwrap().unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "hello_from_middleware");
        });
    }
}
