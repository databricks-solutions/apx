//! Driver↔reactor coordination — suspension, resumption, and task spawning.
//!
//! When the driver suspends on an awaitable, the bridge wires up callbacks
//! so the task gets re-driven when the awaitable resolves.

pub mod queue;

use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::oneshot;

use crate::io::counters;
use crate::io::driver::ffi::{ContextGuard, CoroutineOps};
use crate::io::driver::task::SchedulerTask;
use crate::io::driver::{
    DEFAULT_STEP_BUDGET, DEFAULT_TIME_BUDGET, DriveResult, DriveStats, StepHook, drive_task,
};
use crate::io::reactor::{TaskOps, create_scheduler_task};
use crate::protocol::http::error::AppError;

use self::queue::{ReadyQueue, ReadyTask};

// ---------------------------------------------------------------------------
// TaskContextHook — per-step _enter_task/_leave_task bracket
// ---------------------------------------------------------------------------

/// Per-step task context hook for `_enter_task`/`_leave_task`.
///
/// Constructed in the bridge, passed to `drive_task` via [`StepHook`].
/// Carries cached references — zero per-step Python import overhead.
struct TaskContextHook<'a> {
    enter_task: &'a Py<PyAny>,
    leave_task: &'a Py<PyAny>,
    loop_obj: &'a Py<PyAny>,
    sched_task: &'a Py<PyAny>,
}

impl StepHook for TaskContextHook<'_> {
    fn before_step(&self, py: Python<'_>) {
        if let Err(e) = self.enter_task.call1(py, (self.loop_obj, self.sched_task)) {
            tracing::trace!(error = %e, "step_hook: _enter_task failed (A5 collision)");
        }
    }
    fn after_step(&self, py: Python<'_>) {
        if let Err(e) = self.leave_task.call1(py, (self.loop_obj, self.sched_task)) {
            tracing::trace!(error = %e, "step_hook: _leave_task failed");
        }
    }
}

/// Build a [`TaskContextHook`] from optional sched_task + loop_obj + task_ops.
fn build_step_hook<'a>(
    sched_task: Option<&'a Py<PyAny>>,
    loop_obj: Option<&'a Py<PyAny>>,
    task_ops: &'a TaskOps,
) -> Option<TaskContextHook<'a>> {
    let st = sched_task?;
    let lo = loop_obj?;
    Some(TaskContextHook {
        enter_task: &task_ops.enter_task,
        leave_task: &task_ops.leave_task,
        loop_obj: lo,
        sched_task: st,
    })
}

// ---------------------------------------------------------------------------
// ResumeCallback — re-drives the task when a future resolves
// ---------------------------------------------------------------------------

/// Callback that re-drives a [`SchedulerTask`] after a suspended awaitable
/// resolves.
///
/// Used as `add_done_callback` on asyncio/Rust futures, and as a
/// `call_soon` target for event waiter re-polls and budget-exhausted
/// re-enqueue on the asyncio thread.
///
/// When `drive_inline` is `true`, the callback drives the coroutine
/// directly on the asyncio thread (the thread that fires the done
/// callback during `_run_once`). When `false`, it enqueues to the
/// [`ReadyQueue`] for the drain task.
#[pyclass(module = "apx._core")]
pub struct ResumeCallback {
    task: Option<SchedulerTask>,
    sched_task: Option<Py<PyAny>>,
    queue: Arc<ReadyQueue>,
    ops: Arc<dyn CoroutineOps>,
    call_soon_threadsafe: Py<PyAny>,
    task_ops: TaskOps,
    drive_inline: bool,
}

impl std::fmt::Debug for ResumeCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeCallback")
            .field("has_task", &self.task.is_some())
            .field("drive_inline", &self.drive_inline)
            .finish()
    }
}

#[pymethods]
impl ResumeCallback {
    /// Called by Python when the awaited future completes, or by `call_soon`
    /// for event waiter re-polls.
    ///
    /// When `drive_inline`, drives the coroutine directly on the asyncio
    /// thread — no drain-task GIL hop. Otherwise enqueues to ReadyQueue.
    #[pyo3(signature = (future=None))]
    fn __call__(&mut self, py: Python<'_>, future: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let mut task = self.task.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("ResumeCallback invoked twice")
        })?;

        if let Some(fut) = future {
            apply_future_result(py, &mut task, fut);
        }

        let sched_task = self.sched_task.take();

        if self.drive_inline {
            tracing::trace!("resume_cb: drive inline on asyncio thread");
            return drive_on_loop(
                py,
                task,
                sched_task,
                &self.ops,
                &self.call_soon_threadsafe,
                &self.queue,
                &self.task_ops,
            );
        }

        tracing::trace!("resume_cb: enqueue to ready_queue");
        self.queue.push(py, ReadyTask { task, sched_task });
        Ok(())
    }
}

/// Extract a future's result and apply it to the task's send/throw slot.
fn apply_future_result(py: Python<'_>, task: &mut SchedulerTask, fut: &Bound<'_, PyAny>) {
    match extract_future_result(py, fut) {
        Ok(value) => {
            tracing::trace!("resume_cb: future resolved ok");
            task.set_send_value(value);
        }
        Err(err) => {
            tracing::trace!("resume_cb: future resolved with exception");
            task.set_throw_error(err);
        }
    }
}

/// Drive a task directly on the asyncio thread (inline resume).
///
/// Called from [`ResumeCallback::__call__`] when `drive_inline` is true.
/// Loops when an already-resolved future is detected (avoids stack growth
/// from chains of eager futures on Python 3.12+).
fn drive_on_loop(
    py: Python<'_>,
    mut task: SchedulerTask,
    mut sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
) -> PyResult<()> {
    loop {
        let hook = build_step_hook(sched_task.as_ref(), Some(&task_ops.loop_obj), task_ops);
        let step_hook: Option<&dyn StepHook> = match &hook {
            Some(h) => Some(h),
            None => None,
        };

        let ctx_guard = task
            .ctx
            .as_ref()
            .map(|c| c.clone_ref(py))
            .and_then(ContextGuard::enter);
        let (drive_result, stats) = drive_task(
            py,
            &mut task,
            ops.as_ref(),
            DEFAULT_STEP_BUDGET,
            DEFAULT_TIME_BUDGET,
            step_hook,
        );
        drop(ctx_guard);

        tracing::trace!(steps = stats.steps, "drive_on_loop: iteration done");
        if let Some(c) = counters::get() {
            c.record_drive(&stats);
        }

        match handle_drive_result(
            py,
            task,
            drive_result,
            ops,
            call_soon_threadsafe,
            ready_queue,
            task_ops,
            sched_task,
            true,
        )? {
            HandleOutcome::Done => return Ok(()),
            HandleOutcome::ContinueDriving(t, st) => {
                task = t;
                sched_task = st;
            }
        }
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

/// Outcome of [`handle_drive_result`].
///
/// `Done` means the task has been completed, suspended, or enqueued.
/// `ContinueDriving` means an already-resolved future was detected and
/// the caller should loop back into [`drive_task`] with the returned task.
pub enum HandleOutcome {
    Done,
    ContinueDriving(SchedulerTask, Option<Py<PyAny>>),
}

/// Route a [`DriveResult`] to the appropriate continuation.
///
/// Either completes the task (sending through `result_tx`), or creates a
/// [`ResumeCallback`] and attaches it to the awaitable so the task is
/// re-driven when the awaitable resolves.
///
/// Returns [`HandleOutcome::ContinueDriving`] when a future was already
/// resolved — the caller should loop back into `drive_task`.
///
/// When `on_loop_thread` is `true` (asyncio thread), budget-exhausted and
/// event-wait callbacks use `call_soon` to stay on-loop. Asyncio future
/// callbacks always get `drive_inline=true`; Rust future callbacks always
/// get `drive_inline=false`.
#[expect(
    clippy::too_many_arguments,
    reason = "coordination function wiring driver, reactor, bridge contexts"
)]
pub fn handle_drive_result(
    py: Python<'_>,
    mut task: SchedulerTask,
    drive_result: DriveResult,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
    sched_task: Option<Py<PyAny>>,
    on_loop_thread: bool,
) -> PyResult<HandleOutcome> {
    match drive_result {
        DriveResult::Completed(value) => {
            handle_completed(py, &mut task, value, sched_task.as_ref())?;
            Ok(HandleOutcome::Done)
        }
        DriveResult::Error(err) => {
            handle_error(py, &mut task, err, sched_task.as_ref())?;
            Ok(HandleOutcome::Done)
        }
        DriveResult::WaitingOnFuture(fut) => {
            tracing::trace!("handle_result: waiting on rust Future");
            handle_rust_future(
                py,
                task,
                fut,
                ready_queue,
                sched_task,
                ops,
                call_soon_threadsafe,
                task_ops,
            )
        }
        DriveResult::WaitingOnEvent(_waiter) => {
            handle_event_wait(
                py,
                task,
                sched_task,
                ops,
                call_soon_threadsafe,
                ready_queue,
                task_ops,
                on_loop_thread,
            )?;
            Ok(HandleOutcome::Done)
        }
        DriveResult::WaitingOnAsyncioFuture(fut) => handle_asyncio_future(
            py,
            task,
            fut,
            sched_task,
            ops,
            call_soon_threadsafe,
            ready_queue,
            task_ops,
        ),
        DriveResult::BudgetExhausted => {
            handle_budget_exhausted(
                py,
                task,
                sched_task,
                ops,
                call_soon_threadsafe,
                ready_queue,
                task_ops,
                on_loop_thread,
            )?;
            Ok(HandleOutcome::Done)
        }
    }
}

fn handle_completed(
    py: Python<'_>,
    task: &mut SchedulerTask,
    value: Py<PyAny>,
    sched_task: Option<&Py<PyAny>>,
) -> PyResult<()> {
    tracing::trace!("handle_result: completed");
    if let Some(st) = sched_task {
        let _ = st.call_method0(py, c"cancel");
    }
    if let Some(tx) = task.take_result_tx() {
        let _ = tx.send(Ok(value));
    }
    Ok(())
}

fn handle_error(
    py: Python<'_>,
    task: &mut SchedulerTask,
    err: PyErr,
    sched_task: Option<&Py<PyAny>>,
) -> PyResult<()> {
    tracing::trace!(error = %err, "handle_result: error");
    if let Some(st) = sched_task {
        let _ = st.call_method0(py, c"cancel");
    }
    if let Some(tx) = task.take_result_tx() {
        let _ = tx.send(Err(AppError::Internal(err.to_string())));
    }
    Ok(())
}

/// Asyncio Future — if already resolved, extract result and signal
/// re-drive; otherwise attach a `drive_inline=true` done callback.
#[expect(
    clippy::too_many_arguments,
    reason = "threading context through bridge"
)]
fn handle_asyncio_future(
    py: Python<'_>,
    mut task: SchedulerTask,
    fut: Py<PyAny>,
    sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
) -> PyResult<HandleOutcome> {
    let is_done = fut.call_method0(py, c"done")?.is_truthy(py)?;
    if is_done {
        tracing::trace!("handle_asyncio_future: already done, immediate re-drive");
        apply_future_result(py, &mut task, fut.bind(py));
        return Ok(HandleOutcome::ContinueDriving(task, sched_task));
    }
    tracing::trace!("handle_result: waiting on asyncio Future (drive_inline)");
    let cb = make_resume_callback(
        py,
        task,
        ready_queue,
        sched_task,
        ops,
        call_soon_threadsafe,
        task_ops,
        true,
    )?;
    fut.call_method1(py, c"add_done_callback", (cb,))?;
    Ok(HandleOutcome::Done)
}

/// Event wait — re-poll via `call_soon` (on-loop) or `call_soon_threadsafe`.
#[expect(
    clippy::too_many_arguments,
    reason = "threading context through bridge"
)]
fn handle_event_wait(
    py: Python<'_>,
    task: SchedulerTask,
    sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
    on_loop_thread: bool,
) -> PyResult<()> {
    tracing::trace!("handle_result: waiting on EventWaiter");
    let cb = make_resume_callback(
        py,
        task,
        ready_queue,
        sched_task,
        ops,
        call_soon_threadsafe,
        task_ops,
        true,
    )?;
    if on_loop_thread {
        task_ops.call_soon.call1(py, (cb,))?;
    } else {
        call_soon_threadsafe.call1(py, (cb,))?;
    }
    Ok(())
}

/// Budget exhausted — stay on-loop via `call_soon` or enqueue to ReadyQueue.
#[expect(
    clippy::too_many_arguments,
    reason = "threading context through bridge"
)]
fn handle_budget_exhausted(
    py: Python<'_>,
    task: SchedulerTask,
    sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
    on_loop_thread: bool,
) -> PyResult<()> {
    if on_loop_thread {
        tracing::trace!("handle_result: budget exhausted, call_soon on-loop");
        let cb = make_resume_callback(
            py,
            task,
            ready_queue,
            sched_task,
            ops,
            call_soon_threadsafe,
            task_ops,
            true,
        )?;
        task_ops.call_soon.call1(py, (cb,))?;
    } else {
        tracing::trace!("handle_result: budget exhausted, enqueue to ready_queue");
        ready_queue.push(py, ReadyTask { task, sched_task });
    }
    Ok(())
}

/// Handle `WaitingOnFuture`: if already done, signal re-drive;
/// otherwise attach a done callback with `drive_inline=false` (Rust
/// futures may fire on any thread).
#[expect(
    clippy::too_many_arguments,
    reason = "threading context through bridge"
)]
fn handle_rust_future(
    py: Python<'_>,
    mut task: SchedulerTask,
    fut: Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    task_ops: &TaskOps,
) -> PyResult<HandleOutcome> {
    let is_done = fut.call_method0(py, c"done")?.is_truthy(py)?;
    if is_done {
        tracing::trace!("handle_rust_future: already done, immediate re-drive");
        apply_future_result(py, &mut task, fut.bind(py));
        return Ok(HandleOutcome::ContinueDriving(task, sched_task));
    }
    tracing::trace!("handle_rust_future: pending, attaching done callback (enqueue)");
    let cb = make_resume_callback(
        py,
        task,
        ready_queue,
        sched_task,
        ops,
        call_soon_threadsafe,
        task_ops,
        false,
    )?;
    fut.call_method1(py, c"add_done_callback", (cb,))?;
    Ok(HandleOutcome::Done)
}

// ---------------------------------------------------------------------------
// resume_task — re-drive a task from the ready queue
// ---------------------------------------------------------------------------

/// Re-drive a task that became ready via the queue or inline callback.
///
/// Builds a per-step `TaskContextHook` from the cached `sched_task` and
/// `loop_obj`, calls [`drive_task`] with it, dispatches the result.
/// Loops when an already-resolved future is detected.
///
/// `on_loop_thread` controls how `BudgetExhausted` and events are handled:
/// `true` uses `call_soon` to stay on the asyncio thread; `false` enqueues
/// to the `ReadyQueue` for the drain task.
pub fn resume_task(
    py: Python<'_>,
    ready: ReadyTask,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
    on_loop_thread: bool,
) -> PyResult<()> {
    tracing::trace!(on_loop_thread, "resume_task: entry");
    let ReadyTask {
        mut task,
        mut sched_task,
    } = ready;

    loop {
        let hook = build_step_hook(sched_task.as_ref(), Some(&task_ops.loop_obj), task_ops);
        let step_hook: Option<&dyn StepHook> = match &hook {
            Some(h) => Some(h),
            None => None,
        };

        let ctx_guard = task
            .ctx
            .as_ref()
            .map(|c| c.clone_ref(py))
            .and_then(ContextGuard::enter);
        let (drive_result, stats) = drive_task(
            py,
            &mut task,
            ops.as_ref(),
            DEFAULT_STEP_BUDGET,
            DEFAULT_TIME_BUDGET,
            step_hook,
        );
        drop(ctx_guard);

        tracing::trace!(
            steps = stats.steps,
            budget_exhausted = stats.budget_exhausted,
            "resume_task: drive done"
        );
        if let Some(c) = counters::get() {
            c.record_drive(&stats);
        }

        match handle_drive_result(
            py,
            task,
            drive_result,
            ops,
            call_soon_threadsafe,
            ready_queue,
            task_ops,
            sched_task,
            on_loop_thread,
        )? {
            HandleOutcome::Done => return Ok(()),
            HandleOutcome::ContinueDriving(t, st) => {
                task = t;
                sched_task = st;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// make_resume_callback — factory for ResumeCallback instances
// ---------------------------------------------------------------------------

#[expect(
    clippy::too_many_arguments,
    reason = "factory wiring all context into a single Python callable"
)]
fn make_resume_callback(
    py: Python<'_>,
    task: SchedulerTask,
    ready_queue: &Arc<ReadyQueue>,
    sched_task: Option<Py<PyAny>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    task_ops: &TaskOps,
    drive_inline: bool,
) -> PyResult<Py<ResumeCallback>> {
    Py::new(
        py,
        ResumeCallback {
            task: Some(task),
            sched_task,
            queue: Arc::clone(ready_queue),
            ops: Arc::clone(ops),
            call_soon_threadsafe: call_soon_threadsafe.clone_ref(py),
            task_ops: task_ops.clone_ref(py),
            drive_inline,
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
#[expect(
    clippy::too_many_arguments,
    reason = "scheduler entry point wires driver, reactor, bridge, and poke — a context struct would couple tests to WorkerContext"
)]
pub fn spawn_and_drive(
    py: Python<'_>,
    coro: Py<PyAny>,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    ops: &Arc<dyn CoroutineOps>,
    call_soon_threadsafe: &Py<PyAny>,
    ready_queue: &Arc<ReadyQueue>,
    task_ops: &TaskOps,
    poke_ops: &crate::io::PokeOps,
) -> Option<DriveStats> {
    tracing::trace!("spawn_and_drive: entry");
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

    let n_ready_before = poke_ops.ready_len(py);

    let root_coro = task.root_coro(py);
    let state = create_scheduler_task(py, &root_coro, task_ops);
    let hook = state.as_ref().map(|(lo, st)| TaskContextHook {
        enter_task: &task_ops.enter_task,
        leave_task: &task_ops.leave_task,
        loop_obj: lo,
        sched_task: st,
    });
    let step_hook: Option<&dyn StepHook> = match &hook {
        Some(h) => Some(h),
        None => None,
    };

    let ctx_guard = task
        .ctx
        .as_ref()
        .map(|c| c.clone_ref(py))
        .and_then(ContextGuard::enter);
    let (drive_result, stats) = drive_task(
        py,
        &mut task,
        ops.as_ref(),
        DEFAULT_STEP_BUDGET,
        DEFAULT_TIME_BUDGET,
        step_hook,
    );
    drop(ctx_guard);

    tracing::trace!(
        steps = stats.steps,
        yield_none = stats.yield_none,
        yield_future = stats.yield_future,
        yield_asyncio_future = stats.yield_asyncio_future,
        budget_exhausted = stats.budget_exhausted,
        "spawn_and_drive: first drive done"
    );

    if let Some(c) = counters::get() {
        c.record_drive(&stats);
        match &drive_result {
            DriveResult::Completed(_) | DriveResult::Error(_) => c.record_inline_completion(),
            DriveResult::BudgetExhausted => c.record_budget_exhaustion(),
            _ => c.record_suspension(),
        }
    }

    let needs_poke = !matches!(
        &drive_result,
        DriveResult::Completed(_) | DriveResult::Error(_)
    );

    let sched_task = state.as_ref().map(|(_, st)| st.clone_ref(py));
    match handle_drive_result(
        py,
        task,
        drive_result,
        ops,
        call_soon_threadsafe,
        ready_queue,
        task_ops,
        sched_task,
        false,
    ) {
        Ok(HandleOutcome::ContinueDriving(task, sched_task)) => {
            ready_queue.push(py, ReadyTask { task, sched_task });
        }
        Err(e) => {
            tracing::warn!(error = %e, "scheduler drive result handling failed");
        }
        Ok(HandleOutcome::Done) => {}
    }

    if needs_poke {
        let n_ready_after = poke_ops.ready_len(py);
        poke_ops.maybe_poke(py, n_ready_before, n_ready_after, call_soon_threadsafe);
    }

    Some(stats)
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
    fn resume_callback_debug() {
        crate::with_py(|py| {
            let ready_queue = Arc::new(ReadyQueue::new());
            let ops: Arc<dyn CoroutineOps> =
                Arc::new(crate::io::driver::ffi::FfiCoroutineOps::resolve(py).unwrap());
            let noop = py.eval(c"lambda: None", None, None).unwrap().unbind();
            let task_ops = TaskOps {
                enter_task: noop.clone_ref(py),
                leave_task: noop.clone_ref(py),
                scheduler_task_cls: noop.clone_ref(py),
                call_soon: noop.clone_ref(py),
                loop_obj: noop.clone_ref(py),
            };
            let (tx, _rx) = oneshot::channel();
            let task = SchedulerTask::new(py, py.None(), tx).unwrap();
            let cb = ResumeCallback {
                task: Some(task),
                sched_task: None,
                queue: ready_queue,
                ops,
                call_soon_threadsafe: noop.clone_ref(py),
                task_ops,
                drive_inline: false,
            };
            let dbg = format!("{cb:?}");
            assert!(dbg.contains("ResumeCallback"));
            assert!(dbg.contains("has_task: true"));
            assert!(dbg.contains("drive_inline: false"));
        });
    }
}
