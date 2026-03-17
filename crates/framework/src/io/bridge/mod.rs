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
    DEFAULT_STEP_BUDGET, DEFAULT_TIME_BUDGET, DriveResult, DriveStats, drive_task,
};
use crate::io::reactor::{
    TaskOps, create_scheduler_task, enter_scheduler_task, leave_scheduler_task,
};
use crate::protocol::http::error::AppError;

use self::queue::{ReadyQueue, ReadyTask};

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

        let has_future = future.is_some();
        if let Some(fut) = future {
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

        let sched_task = self.sched_task.take();
        tracing::trace!(
            from_future = has_future,
            "resume_cb: enqueue to ready_queue"
        );
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
            tracing::trace!("handle_result: completed");
            if let Some(ref st) = sched_task {
                let _ = st.call_method0(py, c"cancel");
            }
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Ok(value));
            }
            Ok(())
        }
        DriveResult::Error(err) => {
            tracing::trace!(error = %err, "handle_result: error");
            if let Some(ref st) = sched_task {
                let _ = st.call_method0(py, c"cancel");
            }
            if let Some(tx) = task.take_result_tx() {
                let _ = tx.send(Err(AppError::Internal(err.to_string())));
            }
            Ok(())
        }
        DriveResult::WaitingOnFuture(fut) => {
            tracing::trace!("handle_result: waiting on rust Future");
            handle_rust_future(py, task, fut, ready_queue, sched_task)
        }
        DriveResult::WaitingOnEvent(_waiter) => {
            tracing::trace!("handle_result: waiting on EventWaiter");
            let cb = make_resume_callback(py, task, ready_queue, sched_task)?;
            call_soon_threadsafe.call1(py, (cb,))?;
            Ok(())
        }
        DriveResult::WaitingOnAsyncioFuture(fut) => {
            tracing::trace!("handle_result: waiting on asyncio Future");
            let cb = make_resume_callback(py, task, ready_queue, sched_task)?;
            fut.call_method1(py, c"add_done_callback", (cb,))?;
            Ok(())
        }
        DriveResult::BudgetExhausted => {
            tracing::trace!("handle_result: budget exhausted, re-enqueue");
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
        tracing::trace!("handle_rust_future: already done, immediate re-enqueue");
        let mut task = task;
        match extract_future_result(py, fut.bind(py)) {
            Ok(value) => task.set_send_value(value),
            Err(err) => task.set_throw_error(err),
        }
        ready_queue.push(py, ReadyTask { task, sched_task });
        return Ok(());
    }
    tracing::trace!("handle_rust_future: pending, attaching done callback");
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
    tracing::trace!("resume_task: entry");
    let ReadyTask {
        mut task,
        sched_task,
    } = ready;

    let entered = enter_scheduler_task(py, sched_task.as_ref(), task_ops);

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
    );
    drop(ctx_guard);

    tracing::trace!(
        steps = stats.steps,
        yield_none = stats.yield_none,
        yield_future = stats.yield_future,
        yield_asyncio_future = stats.yield_asyncio_future,
        budget_exhausted = stats.budget_exhausted,
        "resume_task: drive done"
    );

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
}
