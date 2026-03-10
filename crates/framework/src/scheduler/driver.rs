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
use super::task::{SchedulerTask, TaskProxy};
use crate::error::AppError;

// ---------------------------------------------------------------------------
// CachedTypes — pre-resolved Python type references
// ---------------------------------------------------------------------------

/// Pre-resolved Python type references, cached at startup to avoid repeated
/// `import` / `getattr` calls in the hot path.
pub struct CachedTypes {
    /// `asyncio.Future`
    pub asyncio_future: Py<PyType>,
    /// `asyncio.Task`
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "reserved for Task-vs-Future distinction in classify"
        )
    )]
    pub asyncio_task: Py<PyType>,
    /// `types.CoroutineType`
    pub coroutine_type: Py<PyType>,
    /// `types.GeneratorType`
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "reserved for generator-based coroutine detection in classify"
        )
    )]
    pub generator_type: Py<PyType>,
    /// Our `Future` type object.
    pub future_type: Py<PyType>,
    /// Our `EventWaiter` type object.
    pub event_waiter_type: Py<PyType>,
    /// `asyncio.CancelledError` — cached for error creation.
    pub cancelled_error_cls: Py<PyType>,
}

impl std::fmt::Debug for CachedTypes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedTypes").finish_non_exhaustive()
    }
}

impl CachedTypes {
    /// Resolve all Python types once at startup.
    pub fn resolve(py: Python<'_>) -> PyResult<Self> {
        let asyncio = py.import(c"asyncio")?;
        let types = py.import(c"types")?;

        Ok(Self {
            asyncio_future: asyncio.getattr(c"Future")?.cast_into::<PyType>()?.unbind(),
            asyncio_task: asyncio.getattr(c"Task")?.cast_into::<PyType>()?.unbind(),
            coroutine_type: types
                .getattr(c"CoroutineType")?
                .cast_into::<PyType>()?
                .unbind(),
            generator_type: types
                .getattr(c"GeneratorType")?
                .cast_into::<PyType>()?
                .unbind(),
            future_type: Future::type_object(py).unbind(),
            event_waiter_type: EventWaiter::type_object(py).unbind(),
            cancelled_error_cls: asyncio
                .getattr(c"CancelledError")?
                .cast_into::<PyType>()?
                .unbind(),
        })
    }
}

// ---------------------------------------------------------------------------
// StepResult — outcome of advancing a coroutine one step
// ---------------------------------------------------------------------------

/// Outcome of a single `send` / `throw` cycle on a coroutine.
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

/// Advance a coroutine one step by calling `coro.send(value)`.
pub fn step(
    py: Python<'_>,
    coro: &Bound<'_, PyAny>,
    value: Option<&Bound<'_, PyAny>>,
) -> StepResult {
    let send_val = value.map_or_else(|| py.None().into_bound(py), |v| v.clone());
    match coro.call_method1(c"send", (&send_val,)) {
        Ok(yielded) => StepResult::Yielded(yielded.unbind()),
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

/// Advance a coroutine one step by calling `coro.throw(exc)`.
///
/// Python 3.12+ accepts just the exception instance.
pub fn step_throw(py: Python<'_>, coro: &Bound<'_, PyAny>, err: PyErr) -> StepResult {
    match coro.call_method1(c"throw", (err.value(py),)) {
        Ok(yielded) => StepResult::Yielded(yielded.unbind()),
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
    /// Unknown awaitable — fall back to `asyncio.ensure_future`.
    Unknown,
}

// ---------------------------------------------------------------------------
// classify — inspects a yielded object
// ---------------------------------------------------------------------------

/// Classify a yielded value to determine what the driver should do next.
///
/// Check order is optimised for the common case: `yield None` first (36% of
/// all calls), then our own types, then coroutines, then asyncio futures.
pub fn classify(py: Python<'_>, yielded: &Bound<'_, PyAny>, types: &CachedTypes) -> AwaitableKind {
    // Fast path: yield None is the most common case.
    if yielded.is_none() {
        return AwaitableKind::YieldNone;
    }

    // Check our own types first (fast isinstance checks).
    if yielded
        .is_instance(types.future_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::Future;
    }
    if yielded
        .is_instance(types.event_waiter_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::EventWaiter;
    }

    // Check for coroutine (sub-coroutine yield).
    if yielded
        .is_instance(types.coroutine_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::Coroutine;
    }

    // Check for asyncio.Future (includes asyncio.Task since Task extends Future).
    if yielded
        .is_instance(types.asyncio_future.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::AsyncioFuture;
    }

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
    /// Unknown awaitable — fall back to `asyncio.ensure_future`.
    FallbackToAsyncio(Py<PyAny>),
}

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
pub fn drive_task(py: Python<'_>, task: &mut SchedulerTask, types: &CachedTypes) -> DriveResult {
    loop {
        // Obtain step result, dropping the `coro` borrow before we mutate `task`.
        let step_result = {
            let coro = match task.active_coro(py) {
                Ok(c) => c,
                Err(e) => return DriveResult::Error(e),
            };
            if let Some(err) = task.take_throw_error() {
                step_throw(py, &coro, err)
            } else {
                let send_val = task.take_send_value(py);
                step(py, &coro, send_val.as_ref())
            }
        };
        // `coro` is dropped — safe to mutate `task` below.

        match step_result {
            StepResult::Yielded(obj) => {
                let kind = classify(py, obj.bind(py), types);
                match kind {
                    AwaitableKind::YieldNone => continue, // immediate reschedule
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
                        return DriveResult::FallbackToAsyncio(obj);
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
    cached_types: Arc<CachedTypes>,
    result_tx: Option<oneshot::Sender<Result<Py<PyAny>, AppError>>>,
    call_soon: Py<PyAny>,
    ensure_future: Py<PyAny>,
    proxy: Option<Py<TaskProxy>>,
}

impl std::fmt::Debug for ResumeCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeCallback")
            .field("has_task", &self.task.is_some())
            .field("has_result_tx", &self.result_tx.is_some())
            .finish()
    }
}

#[pymethods]
impl ResumeCallback {
    /// Called by Python when the awaited future completes, or by `call_soon`
    /// for event waiter re-polls.
    #[pyo3(signature = (future=None))]
    fn __call__(&mut self, py: Python<'_>, future: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let mut task = self.task.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("ResumeCallback invoked twice")
        })?;
        let result_tx = self.result_tx.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("ResumeCallback result_tx already consumed")
        })?;

        if let Some(fut) = future {
            match extract_future_result(py, fut) {
                Ok(value) => task.set_send_value(value),
                Err(err) => task.set_throw_error(err),
            }
        }

        // Reinstall the proxy as current_task so resumed middleware sees it.
        let current_tasks = install_proxy(py, self.proxy.as_ref());

        let drive_result = drive_task(py, &mut task, &self.cached_types);
        let proxy = self.proxy.take();
        let result = handle_drive_result(
            py,
            task,
            drive_result,
            result_tx,
            &self.cached_types,
            &self.call_soon,
            &self.ensure_future,
            proxy,
        );

        clear_current_task(py, current_tasks);
        result
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
#[expect(
    clippy::too_many_arguments,
    reason = "proxy added for current_task propagation; all params are co-dependent"
)]
fn handle_drive_result(
    py: Python<'_>,
    task: SchedulerTask,
    drive_result: DriveResult,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    cached_types: &Arc<CachedTypes>,
    call_soon: &Py<PyAny>,
    ensure_future: &Py<PyAny>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<()> {
    match drive_result {
        DriveResult::Completed(value) => {
            let _ = result_tx.send(Ok(value));
            Ok(())
        }
        DriveResult::Error(err) => {
            let _ = result_tx.send(Err(AppError::Internal(err.to_string())));
            Ok(())
        }
        DriveResult::WaitingOnFuture(fut) => handle_rust_future(
            py,
            task,
            fut,
            result_tx,
            cached_types,
            call_soon,
            ensure_future,
            proxy,
        ),
        DriveResult::WaitingOnEvent(_waiter) => {
            let cb = make_resume_callback(
                py,
                task,
                result_tx,
                cached_types,
                call_soon,
                ensure_future,
                proxy,
            )?;
            call_soon.call1(py, (cb,))?;
            Ok(())
        }
        DriveResult::WaitingOnAsyncioFuture(fut) => {
            let cb = make_resume_callback(
                py,
                task,
                result_tx,
                cached_types,
                call_soon,
                ensure_future,
                proxy,
            )?;
            fut.call_method1(py, c"add_done_callback", (cb,))?;
            Ok(())
        }
        DriveResult::FallbackToAsyncio(obj) => {
            let asyncio_task = ensure_future.call1(py, (obj,))?;
            let cb = make_resume_callback(
                py,
                task,
                result_tx,
                cached_types,
                call_soon,
                ensure_future,
                proxy,
            )?;
            asyncio_task.call_method1(py, c"add_done_callback", (cb,))?;
            Ok(())
        }
    }
}

/// Handle `WaitingOnFuture`: if already done, re-drive immediately;
/// otherwise attach a done callback.
#[expect(
    clippy::too_many_arguments,
    reason = "proxy added for current_task propagation; all params are co-dependent"
)]
fn handle_rust_future(
    py: Python<'_>,
    task: SchedulerTask,
    fut: Py<PyAny>,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    cached_types: &Arc<CachedTypes>,
    call_soon: &Py<PyAny>,
    ensure_future: &Py<PyAny>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<()> {
    let is_done = fut.call_method0(py, c"done")?.is_truthy(py)?;
    if is_done {
        let mut task = task;
        match extract_future_result(py, fut.bind(py)) {
            Ok(value) => task.set_send_value(value),
            Err(err) => task.set_throw_error(err),
        }
        let drive_result = drive_task(py, &mut task, cached_types);
        return handle_drive_result(
            py,
            task,
            drive_result,
            result_tx,
            cached_types,
            call_soon,
            ensure_future,
            proxy,
        );
    }
    let cb = make_resume_callback(
        py,
        task,
        result_tx,
        cached_types,
        call_soon,
        ensure_future,
        proxy,
    )?;
    fut.call_method1(py, c"add_done_callback", (cb,))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// make_resume_callback — factory for ResumeCallback instances
// ---------------------------------------------------------------------------

fn make_resume_callback(
    py: Python<'_>,
    task: SchedulerTask,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    cached_types: &Arc<CachedTypes>,
    call_soon: &Py<PyAny>,
    ensure_future: &Py<PyAny>,
    proxy: Option<Py<TaskProxy>>,
) -> PyResult<Py<ResumeCallback>> {
    Py::new(
        py,
        ResumeCallback {
            task: Some(task),
            cached_types: Arc::clone(cached_types),
            result_tx: Some(result_tx),
            call_soon: call_soon.clone_ref(py),
            ensure_future: ensure_future.clone_ref(py),
            proxy,
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
    ensure_future: &Py<PyAny>,
) {
    let mut task = match SchedulerTask::new(py, coro) {
        Ok(t) => t,
        Err(e) => {
            let _ = result_tx.send(Err(AppError::Internal(format!("task creation: {e}"))));
            return;
        }
    };

    // Set our task as asyncio's "current task" so Starlette/FastAPI
    // middleware that calls asyncio.current_task() gets a valid object
    // (needed for weakref support in ServerErrorMiddleware, etc.).
    let task_ctx = set_current_task(py, &task);

    let drive_result = drive_task(py, &mut task, cached_types);
    let proxy = task_ctx.as_ref().map(|(_, p)| p.clone_ref(py));
    if let Err(e) = handle_drive_result(
        py,
        task,
        drive_result,
        result_tx,
        cached_types,
        call_soon,
        ensure_future,
        proxy,
    ) {
        tracing::warn!(error = %e, "scheduler drive result handling failed");
    }

    clear_current_task(py, task_ctx.map(|(ct, _)| ct));
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

    let proxy = Py::new(
        py,
        TaskProxy::new(
            task.result_future.clone_ref(py),
            loop_obj.clone().unbind(),
            task.root_coro(py),
        ),
    )
    .ok()?;

    let _ = current_tasks.call_method1(c"__setitem__", (&loop_obj, &proxy));
    Some((current_tasks.unbind(), proxy))
}

/// Reinstall an existing [`TaskProxy`] in `asyncio.tasks._current_tasks`.
///
/// Used by `ResumeCallback` to restore the current task when re-driving.
/// Returns the `_current_tasks` dict for cleanup, or `None` if unavailable.
fn install_proxy(py: Python<'_>, proxy: Option<&Py<TaskProxy>>) -> Option<Py<PyAny>> {
    let proxy = proxy?;
    let tasks_mod = py.import(c"asyncio.tasks").ok()?;
    let current_tasks = tasks_mod.getattr(c"_current_tasks").ok()?;
    let asyncio = py.import(c"asyncio").ok()?;
    let loop_obj = asyncio.call_method0(c"get_running_loop").ok()?;
    let _ = current_tasks.call_method1(c"__setitem__", (&loop_obj, proxy));
    Some(current_tasks.unbind())
}

/// Remove our task from `asyncio._current_tasks`.
fn clear_current_task(py: Python<'_>, current_tasks: Option<Py<PyAny>>) {
    let Some(ct) = current_tasks else { return };
    let Ok(asyncio) = py.import(c"asyncio") else {
        return;
    };
    let Ok(loop_obj) = asyncio.call_method0(c"get_running_loop") else {
        return;
    };
    let _ = ct.call_method1(py, c"pop", (&loop_obj, py.None()));
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
            assert!(!types.asyncio_future.bind(py).is_none());
            assert!(!types.asyncio_task.bind(py).is_none());
            assert!(!types.coroutine_type.bind(py).is_none());
            assert!(!types.generator_type.bind(py).is_none());
            assert!(!types.future_type.bind(py).is_none());
            assert!(!types.event_waiter_type.bind(py).is_none());
        });
    }

    #[test]
    fn classify_none_is_yield_none() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            let none = py.None().into_bound(py);
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
            let py_future = Py::new(py, future).unwrap();
            let bound = py_future.into_bound(py).into_any();
            assert!(matches!(
                classify(py, &bound, &types),
                AwaitableKind::Future
            ));
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
            let coro = py.eval(c"coro()", None, None).unwrap();
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
            let coro = py.eval(c"coro()", None, None).unwrap();
            let err = pyo3::exceptions::PyValueError::new_err("test error");
            let result = step_throw(py, &coro, err);
            assert!(matches!(result, StepResult::Error(_)));
        });
    }

    // -- spawn_and_drive tests -----------------------------------------------

    /// Helper: create dummy `call_soon` and `ensure_future` refs for tests
    /// where they won't actually be called (trivial coroutines).
    fn dummy_loop_fns(py: Python<'_>) -> (Py<PyAny>, Py<PyAny>) {
        let noop = py.eval(c"lambda *a, **kw: None", None, None).unwrap();
        (noop.clone().unbind(), noop.unbind())
    }

    #[test]
    fn spawn_and_drive_trivial_coroutine() {
        crate::with_py(|py| {
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let (call_soon, ensure_future) = dummy_loop_fns(py);

            py.run(c"async def _f(): return 42", None, None).unwrap();
            let coro = py.eval(c"_f()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            spawn_and_drive(py, coro, tx, &types, &call_soon, &ensure_future);

            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn spawn_and_drive_coroutine_error() {
        crate::with_py(|py| {
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let (call_soon, ensure_future) = dummy_loop_fns(py);

            py.run(c"async def _err(): raise ValueError('boom')", None, None)
                .unwrap();
            let coro = py.eval(c"_err()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            spawn_and_drive(py, coro, tx, &types, &call_soon, &ensure_future);

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
            let types = Arc::new(CachedTypes::resolve(py).unwrap());
            let (call_soon, ensure_future) = dummy_loop_fns(py);
            let (tx, _rx) = oneshot::channel();

            let task = SchedulerTask::new(py, py.None()).unwrap();
            let cb = ResumeCallback {
                task: Some(task),
                cached_types: types,
                result_tx: Some(tx),
                call_soon,
                ensure_future,
                proxy: None,
            };
            let dbg = format!("{cb:?}");
            assert!(dbg.contains("ResumeCallback"));
            assert!(dbg.contains("has_task: true"));
            assert!(dbg.contains("has_result_tx: true"));
        });
    }
}
