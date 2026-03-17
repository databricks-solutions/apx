//! Coroutine driver — the core innovation of the Rust scheduler.
//!
//! Replaces `asyncio.Task._step` by calling `coro.send(value)` directly from
//! Rust and interpreting yielded values to decide what to do next.
//!
//! The most common case (36% of all primitive calls) is `yield None` — the
//! driver handles this by looping immediately without any
//! Python→asyncio→Python round trip.

use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::oneshot;

use super::queue::{ReadyQueue, ReadyTask};
use super::task::SchedulerTask;
use crate::ffi::{AwaitableKind, CoroutineOps, StepResult};
use crate::protocol::http::error::AppError;
use crate::scheduler::counters;

// ---------------------------------------------------------------------------
// TaskOps — cached Python callables for scheduler task lifecycle
// ---------------------------------------------------------------------------

/// Cached Python callables for the scheduler task lifecycle.
///
/// Resolved once at worker init, passed by reference to avoid per-call imports.
pub struct TaskOps {
    pub enter_task: Py<PyAny>,
    pub leave_task: Py<PyAny>,
    pub scheduler_task_cls: Py<PyAny>,
}

impl std::fmt::Debug for TaskOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskOps").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// DriveStats — per-request scheduler drive counters
// ---------------------------------------------------------------------------

/// Counters collected during a single `drive_task` invocation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DriveStats {
    pub steps: u32,
    pub yield_none: u32,
    pub yield_future: u32,
    pub yield_asyncio_future: u32,
    pub yield_coroutine: u32,
    pub yield_unknown: u32,
    pub budget_exhausted: bool,
}

// ---------------------------------------------------------------------------
// DriveResult — outcome of driving a task
// ---------------------------------------------------------------------------

/// Outcome of driving a [`SchedulerTask`] until it suspends or completes.
#[derive(Debug)]
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
    ops: &dyn CoroutineOps,
    step_budget: usize,
) -> (DriveResult, DriveStats) {
    let mut stats = DriveStats::default();
    loop {
        // Clone the coro ref so the immutable borrow on `task` is released
        // before we mutate it (take_throw_error / take_send_value).
        let coro = match task.active_coro() {
            Ok(c) => c.clone_ref(py),
            Err(e) => return (DriveResult::Error(e), stats),
        };
        let step_result = if let Some(err) = task.take_throw_error() {
            ops.step_throw(py, &coro, err)
        } else {
            let send_val = task.take_send_value();
            ops.step(py, &coro, send_val.as_ref())
        };

        match step_result {
            StepResult::Yielded(obj) => {
                let kind = ops.classify(py, &obj);
                match kind {
                    AwaitableKind::YieldNone => {
                        stats.steps += 1;
                        stats.yield_none += 1;
                        if stats.steps as usize >= step_budget {
                            stats.budget_exhausted = true;
                            return (DriveResult::BudgetExhausted, stats);
                        }
                        continue;
                    }
                    AwaitableKind::Future => {
                        stats.yield_future += 1;
                        return (DriveResult::WaitingOnFuture(obj), stats);
                    }
                    AwaitableKind::EventWaiter => {
                        return (DriveResult::WaitingOnEvent(obj), stats);
                    }
                    AwaitableKind::Coroutine => {
                        stats.yield_coroutine += 1;
                        task.push_coro(obj);
                        continue; // drive the sub-coroutine
                    }
                    AwaitableKind::AsyncioFuture => {
                        stats.yield_asyncio_future += 1;
                        return (DriveResult::WaitingOnAsyncioFuture(obj), stats);
                    }
                    AwaitableKind::Unknown => {
                        stats.yield_unknown += 1;
                        let type_name = obj
                            .bind(py)
                            .get_type()
                            .name()
                            .map_or_else(|_| "<unknown>".to_owned(), |n| n.to_string());
                        return (
                            DriveResult::Error(pyo3::exceptions::PyTypeError::new_err(format!(
                                "unsupported awaitable type yielded: {type_name}"
                            ))),
                            stats,
                        );
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
                return (DriveResult::Completed(value), stats);
            }
            StepResult::Error(e) => {
                if task.pop_coro() {
                    // Sub-coroutine raised — throw into parent.
                    task.set_throw_error(e);
                    continue;
                }
                return (DriveResult::Error(e), stats);
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
    sched_task: Option<Py<PyAny>>,
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

        let sched_task = self.sched_task.take();
        tracing::trace!("resume_callback: enqueue");
        self.queue.push(py, ReadyTask { task, sched_task });
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
pub fn handle_drive_result(
    py: Python<'_>,
    mut task: SchedulerTask,
    drive_result: DriveResult,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    sched_task: Option<Py<PyAny>>,
) -> PyResult<()> {
    match drive_result {
        DriveResult::Completed(value) => {
            if let Some(ref st) = sched_task {
                let _ = st.call_method0(py, c"cancel");
            }
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Ok(value));
            }
            Ok(())
        }
        DriveResult::Error(err) => {
            if let Some(ref st) = sched_task {
                let _ = st.call_method0(py, c"cancel");
            }
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Err(AppError::Internal(err.to_string())));
            }
            Ok(())
        }
        DriveResult::WaitingOnFuture(fut) => {
            handle_rust_future(py, task, fut, ready_queue, sched_task)
        }
        DriveResult::WaitingOnEvent(_waiter) => {
            let cb = make_resume_callback(py, task, ready_queue, sched_task)?;
            call_soon_threadsafe.call1(py, (cb,))?;
            Ok(())
        }
        DriveResult::WaitingOnAsyncioFuture(fut) => {
            let cb = make_resume_callback(py, task, ready_queue, sched_task)?;
            fut.call_method1(py, c"add_done_callback", (cb,))?;
            Ok(())
        }
        DriveResult::BudgetExhausted => {
            ready_queue.push(py, ReadyTask { task, sched_task });
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
    sched_task: Option<Py<PyAny>>,
) -> PyResult<()> {
    let is_done = fut.call_method0(py, c"done")?.is_truthy(py)?;
    if is_done {
        let mut task = task;
        match extract_future_result(py, fut.bind(py)) {
            Ok(value) => task.set_send_value(value),
            Err(err) => task.set_throw_error(err),
        }
        ready_queue.push(py, ReadyTask { task, sched_task });
        return Ok(());
    }
    let cb = make_resume_callback(py, task, ready_queue, sched_task)?;
    fut.call_method1(py, c"add_done_callback", (cb,))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// resume_task — re-drive a task from the ready queue
// ---------------------------------------------------------------------------

/// Re-drive a task that became ready via the queue.
///
/// Re-enters the `_SchedulerTask`, calls [`drive_task`], dispatches the result.
/// `result_tx` is inside `task` — no separate channel parameter.
pub fn resume_task(
    py: Python<'_>,
    ready: ReadyTask,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
) -> PyResult<()> {
    let ReadyTask {
        mut task,
        sched_task,
    } = ready;

    let entered = enter_scheduler_task(py, sched_task.as_ref(), task_ops);

    let (drive_result, stats) = drive_task(py, &mut task, ops.as_ref(), DEFAULT_STEP_BUDGET);
    if let Some(c) = counters::get() {
        c.record_drive(&stats);
    }
    let result = handle_drive_result(
        py,
        task,
        drive_result,
        call_soon_threadsafe,
        ready_queue,
        sched_task,
    );

    leave_scheduler_task(py, entered, task_ops);
    result
}

// ---------------------------------------------------------------------------
// make_resume_callback — factory for ResumeCallback instances
// ---------------------------------------------------------------------------

fn make_resume_callback(
    py: Python<'_>,
    task: SchedulerTask,
    ready_queue: &Arc<ReadyQueue>,
    sched_task: Option<Py<PyAny>>,
) -> PyResult<Py<ResumeCallback>> {
    Py::new(
        py,
        ResumeCallback {
            task: Some(task),
            sched_task,
            queue: Arc::clone(ready_queue),
        },
    )
}

/// Create a `_SchedulerTask` and register it as the current asyncio task.
fn create_scheduler_task(
    py: Python<'_>,
    task: &SchedulerTask,
    ops: &TaskOps,
) -> Option<(Py<PyAny>, Py<PyAny>)> {
    let asyncio = py.import(c"asyncio").ok()?;
    let loop_obj = asyncio.call_method0(c"get_running_loop").ok()?;
    let coro = task.root_coro(py);
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("loop", &loop_obj).ok()?;
    let sched_task = ops
        .scheduler_task_cls
        .call(py, (coro,), Some(&kwargs))
        .ok()?;
    ops.enter_task.call1(py, (&loop_obj, &sched_task)).ok()?;
    Some((loop_obj.unbind(), sched_task))
}

/// Re-enter an existing `_SchedulerTask` as the current asyncio task.
fn enter_scheduler_task(
    py: Python<'_>,
    sched_task: Option<&Py<PyAny>>,
    ops: &TaskOps,
) -> Option<(Py<PyAny>, Py<PyAny>)> {
    let sched_task = sched_task?;
    let asyncio = py.import(c"asyncio").ok()?;
    let loop_obj = asyncio.call_method0(c"get_running_loop").ok()?;
    ops.enter_task.call1(py, (&loop_obj, sched_task)).ok()?;
    Some((loop_obj.unbind(), sched_task.clone_ref(py)))
}

/// Unregister the current `_SchedulerTask` via `_leave_task`.
fn leave_scheduler_task(py: Python<'_>, state: Option<(Py<PyAny>, Py<PyAny>)>, ops: &TaskOps) {
    let Some((loop_obj, sched_task)) = state else {
        return;
    };
    if let Err(e) = ops.leave_task.call1(py, (&loop_obj, &sched_task)) {
        tracing::warn!(error = %e, "leave_scheduler_task: _leave_task failed");
    }
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
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
) -> Option<DriveStats> {
    let mut task = match SchedulerTask::new(py, coro, result_tx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler task creation failed");
            return None;
        }
    };

    if let Some(c) = counters::get() {
        c.record_spawn();
    }

    // Set our task as asyncio's "current task" so Starlette/FastAPI
    // middleware that calls asyncio.current_task() gets a valid object
    // (needed for weakref support in ServerErrorMiddleware, etc.).
    let state = create_scheduler_task(py, &task, task_ops);

    let (drive_result, stats) = drive_task(py, &mut task, ops.as_ref(), DEFAULT_STEP_BUDGET);

    // Record counters based on drive result.
    if let Some(c) = counters::get() {
        c.record_drive(&stats);
        match &drive_result {
            DriveResult::Completed(_) | DriveResult::Error(_) => c.record_inline_completion(),
            DriveResult::BudgetExhausted => c.record_budget_exhaustion(),
            _ => c.record_suspension(),
        }
    }

    let sched_task = state.as_ref().map(|(_, st)| st.clone_ref(py));
    if let Err(e) = handle_drive_result(
        py,
        task,
        drive_result,
        call_soon_threadsafe,
        ready_queue,
        sched_task,
    ) {
        tracing::warn!(error = %e, "scheduler drive result handling failed");
    }

    leave_scheduler_task(py, state, task_ops);
    Some(stats)
}

// ---------------------------------------------------------------------------
// first_drive — inline completion (test-only, kept for scheduler unit tests)
// ---------------------------------------------------------------------------

/// Outcome of driving a coroutine's first cycle on the driver thread.
#[cfg(test)]
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "fields used for pattern matching and Debug output in tests"
)]
pub enum FirstDriveOutcome {
    /// Completed or errored inline — result already sent via `result_tx`.
    Inline,
    /// Suspended on an awaitable — event loop thread must attach continuation.
    Suspended {
        task: Box<SchedulerTask>,
        sched_task: Option<Py<PyAny>>,
        drive_result: DriveResult,
    },
}

/// Drive a coroutine's first cycle. Completes trivial coros inline;
/// returns suspended state for the event loop to handle.
///
/// Skips task registration — drive mechanics don't depend on it.
#[cfg(test)]
pub fn first_drive(
    py: Python<'_>,
    coro: Py<PyAny>,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    ops: &Arc<dyn CoroutineOps>,
) -> FirstDriveOutcome {
    let mut task = match SchedulerTask::new(py, coro, result_tx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "first_drive: task creation failed");
            return FirstDriveOutcome::Inline;
        }
    };
    let (drive_result, _stats) = drive_task(py, &mut task, ops.as_ref(), DEFAULT_STEP_BUDGET);
    route_first_drive(py, task, drive_result)
}

/// Route the drive result: complete inline or return suspended state.
#[cfg(test)]
fn route_first_drive(
    _py: Python<'_>,
    mut task: SchedulerTask,
    drive_result: DriveResult,
) -> FirstDriveOutcome {
    match drive_result {
        DriveResult::Completed(value) => {
            complete_inline(&mut task, Ok(value));
            FirstDriveOutcome::Inline
        }
        DriveResult::Error(err) => {
            complete_inline(&mut task, Err(AppError::Internal(err.to_string())));
            FirstDriveOutcome::Inline
        }
        result => FirstDriveOutcome::Suspended {
            task: Box::new(task),
            sched_task: None,
            drive_result: result,
        },
    }
}

/// Send result through the oneshot channel for inline completion.
#[cfg(test)]
fn complete_inline(task: &mut SchedulerTask, result: Result<Py<PyAny>, AppError>) {
    if let Some(tx) = task.take_result_tx() {
        let _ = tx.send(result);
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
    use crate::ffi::FfiCoroutineOps;

    // -- first_drive tests -----------------------------------------------

    #[test]
    fn first_drive_trivial_coroutine() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(c"async def _f(): return 42", None, None).unwrap();
            let coro = py.eval(c"_f()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn first_drive_coroutine_error() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(c"async def _err(): raise ValueError('boom')", None, None)
                .unwrap();
            let coro = py.eval(c"_err()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

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
    fn first_drive_suspended_coroutine() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            // Create a coroutine that suspends on an asyncio.Future.
            let asyncio = py.import(c"asyncio").unwrap();
            let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
            let events = py.import(c"asyncio.events").unwrap();
            let _ = events.call_method1(c"_set_running_loop", (&loop_obj,));

            py.run(
                c"
import asyncio
async def _suspend():
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    await fut
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"_suspend()", None, None).unwrap().unbind();

            let (tx, _rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(
                matches!(outcome, FirstDriveOutcome::Suspended { .. }),
                "expected Suspended, got {outcome:?}"
            );

            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let _ = loop_obj.call_method0(c"close");
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
                sched_task: None,
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
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            // Set a contextvar before first_drive, verify it's visible
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
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

            let result = rx.try_recv().unwrap().unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "hello_from_middleware");
        });
    }
}
