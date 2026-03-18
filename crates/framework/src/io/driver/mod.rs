//! Coroutine driver — the core innovation of the Rust scheduler.
//!
//! Replaces `asyncio.Task._step` by calling `coro.send(value)` directly from
//! Rust and interpreting yielded values to decide what to do next.
//!
//! The most common case (36% of all primitive calls) is `yield None` — the
//! driver handles this by looping immediately without any
//! Python→asyncio→Python round trip.

pub mod ffi;
pub mod primitives;
pub mod task;

use std::time::{Duration, Instant};

use pyo3::prelude::*;

use self::ffi::{AwaitableKind, CoroutineOps, StepResult};
use self::task::SchedulerTask;

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
    pub time_budget_exceeded: bool,
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
pub const DEFAULT_STEP_BUDGET: usize = 128;

/// Maximum wall-clock time per drive cycle before re-enqueue.
///
/// Prevents GIL starvation when user coroutines do CPU-intensive work.
/// The reactor thread needs GIL to run `_run_once` — holding it longer
/// than this starves I/O processing.
pub const DEFAULT_TIME_BUDGET: Duration = Duration::from_millis(5);

/// Steps between wall-clock time checks.
///
/// `Instant::elapsed()` costs ~25ns — checking every 16 steps
/// amortizes this to ~1.5ns/step.
const TIME_CHECK_INTERVAL: u32 = 16;

// ---------------------------------------------------------------------------
// StepHook — per-step callback for task context bracketing
// ---------------------------------------------------------------------------

/// Hook for per-step callbacks around Python bytecode execution.
///
/// Injected by the bridge to bracket each step+classify cycle with
/// `_enter_task`/`_leave_task`. The driver has no dependency on reactor
/// or asyncio types — this trait is the abstraction boundary.
pub trait StepHook {
    /// Called before each step+classify cycle (enters task context).
    fn before_step(&self, py: Python<'_>);
    /// Called after each step+classify cycle (leaves task context).
    fn after_step(&self, py: Python<'_>);
}

/// Outcome of a single step+classify cycle within the drive loop.
enum StepOutcome {
    /// Internal continuation — coroutine push/pop, no budget check.
    Continue,
    /// Yielded None — budget check required before re-entering.
    YieldedNone,
    /// Terminal — exit drive loop with this result.
    Done(DriveResult),
}

// ---------------------------------------------------------------------------
// drive_task — the main drive loop
// ---------------------------------------------------------------------------

/// Drive a [`SchedulerTask`] until it suspends or completes.
///
/// This is the hot loop. When the yielded object is `None` (most common case),
/// it immediately loops without suspending. Sub-coroutines are pushed onto the
/// task's coroutine stack and driven inline.
///
/// When `step_hook` is provided, each step+classify cycle is bracketed by
/// `before_step`/`after_step` calls. Budget checks (pure Rust) run outside
/// the bracket — no `_enter_task` held during GIL-safe code.
#[expect(
    clippy::needless_continue,
    reason = "explicit `continue` documents intent to re-enter the drive loop"
)]
pub fn drive_task(
    py: Python<'_>,
    task: &mut SchedulerTask,
    ops: &dyn CoroutineOps,
    step_budget: usize,
    time_budget: Duration,
    step_hook: Option<&dyn StepHook>,
) -> (DriveResult, DriveStats) {
    let start = Instant::now();
    let mut stats = DriveStats::default();
    loop {
        if let Some(hook) = step_hook {
            hook.before_step(py);
        }
        let outcome = step_and_classify(py, task, ops, &mut stats);
        if let Some(hook) = step_hook {
            hook.after_step(py);
        }
        match outcome {
            StepOutcome::Continue => continue,
            StepOutcome::YieldedNone => {
                if budget_exceeded(&start, &mut stats, step_budget, time_budget) {
                    return (DriveResult::BudgetExhausted, stats);
                }
            }
            StepOutcome::Done(result) => return (result, stats),
        }
    }
}

/// Execute one step + classify cycle. All Python bytecode runs here.
fn step_and_classify(
    py: Python<'_>,
    task: &mut SchedulerTask,
    ops: &dyn CoroutineOps,
    stats: &mut DriveStats,
) -> StepOutcome {
    let coro = match task.active_coro() {
        Ok(c) => c.clone_ref(py),
        Err(e) => return StepOutcome::Done(DriveResult::Error(e)),
    };
    let step_result = if let Some(err) = task.take_throw_error() {
        ops.step_throw(py, &coro, err)
    } else {
        let send_val = task.take_send_value();
        ops.step(py, &coro, send_val.as_ref())
    };
    match step_result {
        StepResult::Yielded(obj) => classify_yielded(py, ops, task, stats, obj),
        StepResult::Completed(value) => route_completed(task, stats, value),
        StepResult::Error(e) => route_error(task, stats, e),
    }
}

/// Classify a yielded value and determine the step outcome.
fn classify_yielded(
    py: Python<'_>,
    ops: &dyn CoroutineOps,
    task: &mut SchedulerTask,
    stats: &mut DriveStats,
    obj: Py<PyAny>,
) -> StepOutcome {
    match ops.classify(py, &obj) {
        AwaitableKind::YieldNone => {
            stats.steps += 1;
            stats.yield_none += 1;
            StepOutcome::YieldedNone
        }
        AwaitableKind::Future => {
            stats.yield_future += 1;
            tracing::trace!(steps = stats.steps, "drive: suspend on Future");
            StepOutcome::Done(DriveResult::WaitingOnFuture(obj))
        }
        AwaitableKind::EventWaiter => {
            tracing::trace!(steps = stats.steps, "drive: suspend on EventWaiter");
            StepOutcome::Done(DriveResult::WaitingOnEvent(obj))
        }
        AwaitableKind::Coroutine => {
            stats.yield_coroutine += 1;
            task.push_coro(obj);
            StepOutcome::Continue
        }
        AwaitableKind::AsyncioFuture => {
            stats.yield_asyncio_future += 1;
            tracing::trace!(steps = stats.steps, "drive: suspend on AsyncioFuture");
            StepOutcome::Done(DriveResult::WaitingOnAsyncioFuture(obj))
        }
        AwaitableKind::CustomAwaitable => match obj.call_method0(py, c"__await__") {
            Ok(iter) => {
                stats.yield_coroutine += 1;
                task.push_coro(iter);
                StepOutcome::Continue
            }
            Err(e) => StepOutcome::Done(DriveResult::Error(e)),
        },
        AwaitableKind::Unknown => {
            stats.yield_unknown += 1;
            let type_name = obj
                .bind(py)
                .get_type()
                .name()
                .map_or_else(|_| "<unknown>".to_owned(), |n| n.to_string());
            tracing::trace!(steps = stats.steps, %type_name, "drive: unknown awaitable");
            StepOutcome::Done(DriveResult::Error(pyo3::exceptions::PyTypeError::new_err(
                format!("unsupported awaitable type yielded: {type_name}"),
            )))
        }
    }
}

/// Route a completed step — pop coro stack or signal completion.
fn route_completed(task: &mut SchedulerTask, stats: &DriveStats, value: Py<PyAny>) -> StepOutcome {
    if task.pop_coro() {
        task.set_send_value(value);
        return StepOutcome::Continue;
    }
    tracing::trace!(
        steps = stats.steps,
        yield_none = stats.yield_none,
        yield_future = stats.yield_future,
        yield_asyncio_future = stats.yield_asyncio_future,
        "drive: completed"
    );
    StepOutcome::Done(DriveResult::Completed(value))
}

/// Route an error step — pop coro stack or signal error.
fn route_error(task: &mut SchedulerTask, stats: &DriveStats, err: PyErr) -> StepOutcome {
    if task.pop_coro() {
        task.set_throw_error(err);
        return StepOutcome::Continue;
    }
    tracing::trace!(steps = stats.steps, error = %err, "drive: error");
    StepOutcome::Done(DriveResult::Error(err))
}

/// Check if the step or time budget is exhausted.
fn budget_exceeded(
    start: &Instant,
    stats: &mut DriveStats,
    step_budget: usize,
    time_budget: Duration,
) -> bool {
    if stats.steps as usize >= step_budget {
        stats.budget_exhausted = true;
        tracing::trace!(steps = stats.steps, "drive: budget exhausted (step limit)");
        return true;
    }
    if stats.steps.is_multiple_of(TIME_CHECK_INTERVAL) && start.elapsed() > time_budget {
        stats.budget_exhausted = true;
        stats.time_budget_exceeded = true;
        tracing::trace!(
            steps = stats.steps,
            elapsed_us = start.elapsed().as_micros() as u64,
            "drive: budget exhausted (time)"
        );
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// first_drive — inline completion (test-only, kept for scheduler unit tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
use self::ffi::ContextGuard;
#[cfg(test)]
use crate::protocol::http::error::AppError;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::oneshot;

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
    let ctx_guard = task
        .ctx
        .as_ref()
        .map(|c| c.clone_ref(py))
        .and_then(ContextGuard::enter);
    let (drive_result, _stats) = drive_task(
        py,
        &mut task,
        ops.as_ref(),
        DEFAULT_STEP_BUDGET,
        DEFAULT_TIME_BUDGET,
        None,
    );
    drop(ctx_guard);
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
    use super::ffi::FfiCoroutineOps;
    use super::*;

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

    #[test]
    fn drive_custom_awaitable() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(
                c"
class CustomAwaitable:
    def __await__(self):
        yield None  # suspend once
        return 42

async def uses_custom():
    return await CustomAwaitable()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"uses_custom()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            // CustomAwaitable yields None once, so the driver suspends.
            assert!(
                matches!(outcome, FirstDriveOutcome::Inline),
                "expected Inline (yield None loops then __await__ returns), got {outcome:?}"
            );
            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn custom_awaitable_bad_await_raises() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(
                c"
class BadAwait:
    def __await__(self):
        return 42  # not an iterator

async def uses_bad():
    return await BadAwait()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"uses_bad()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            // Should complete inline with an error (TypeError from non-iterator).
            assert!(matches!(outcome, FirstDriveOutcome::Inline));
            let result = rx.try_recv().unwrap();
            assert!(result.is_err(), "expected error from bad __await__, got Ok");
        });
    }

    // -- drive cycle tests ---------------------------------------------------

    #[test]
    fn drive_deeply_nested_coroutines() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            py.run(
                c"
async def level(n):
    if n == 0:
        return 'bottom'
    return await level(n - 1)
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"level(10)", None, None).unwrap().unbind();
            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));
            let result = rx.try_recv().unwrap().unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "bottom");
        });
    }

    #[test]
    fn drive_budget_exhaustion() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            py.run(
                c"
class ManyYields:
    def __await__(self):
        for _ in range(200):
            yield None
        return 'done'

async def exhaust_budget():
    return await ManyYields()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"exhaust_budget()", None, None).unwrap().unbind();
            let (tx, _rx) = oneshot::channel();
            let mut task = SchedulerTask::new(py, coro, tx).unwrap();
            let (result, stats) = drive_task(
                py,
                &mut task,
                ops.as_ref(),
                DEFAULT_STEP_BUDGET,
                DEFAULT_TIME_BUDGET,
                None,
            );
            assert!(matches!(result, DriveResult::BudgetExhausted));
            assert!(stats.budget_exhausted);
            assert!(stats.steps as usize >= DEFAULT_STEP_BUDGET);
        });
    }

    #[test]
    fn drive_asyncio_future_suspends() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            let asyncio = py.import(c"asyncio").unwrap();
            let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
            let events = py.import(c"asyncio.events").unwrap();
            let _ = events.call_method1(c"_set_running_loop", (&loop_obj,));
            py.run(
                c"
import asyncio
async def wait_for_future():
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    await fut
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"wait_for_future()", None, None).unwrap().unbind();
            let (tx, _rx) = oneshot::channel();
            let mut task = SchedulerTask::new(py, coro, tx).unwrap();
            let (result, _stats) = drive_task(
                py,
                &mut task,
                ops.as_ref(),
                DEFAULT_STEP_BUDGET,
                DEFAULT_TIME_BUDGET,
                None,
            );
            assert!(matches!(result, DriveResult::WaitingOnAsyncioFuture(_)));
            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let _ = loop_obj.call_method0(c"close");
        });
    }

    #[test]
    fn drive_rust_future_suspends() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            use crate::io::driver::primitives::Future as RustFuture;
            let pending = Py::new(py, RustFuture::pending()).unwrap();
            let globals = pyo3::types::PyDict::new(py);
            globals.set_item("rust_fut", &pending).unwrap();
            py.run(
                c"
async def wait_rust():
    return await rust_fut
",
                Some(&globals),
                None,
            )
            .unwrap();
            let coro = py
                .eval(c"wait_rust()", Some(&globals), None)
                .unwrap()
                .unbind();
            let (tx, _rx) = oneshot::channel();
            let mut task = SchedulerTask::new(py, coro, tx).unwrap();
            let (result, _stats) = drive_task(
                py,
                &mut task,
                ops.as_ref(),
                DEFAULT_STEP_BUDGET,
                DEFAULT_TIME_BUDGET,
                None,
            );
            assert!(
                matches!(result, DriveResult::WaitingOnFuture(_)),
                "expected WaitingOnFuture, got {result:?}"
            );
        });
    }

    #[test]
    fn drive_concurrent_via_ready_queue() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            // Task 1: completes inline
            py.run(c"async def t1(): return 'one'", None, None).unwrap();
            let coro1 = py.eval(c"t1()", None, None).unwrap().unbind();
            let (tx1, mut rx1) = oneshot::channel();
            let o1 = first_drive(py, coro1, tx1, &ops);
            assert!(matches!(o1, FirstDriveOutcome::Inline));
            let r1: String = rx1.try_recv().unwrap().unwrap().extract(py).unwrap();
            // Task 2: also completes inline
            py.run(c"async def t2(): return 'two'", None, None).unwrap();
            let coro2 = py.eval(c"t2()", None, None).unwrap().unbind();
            let (tx2, mut rx2) = oneshot::channel();
            let o2 = first_drive(py, coro2, tx2, &ops);
            assert!(matches!(o2, FirstDriveOutcome::Inline));
            let r2: String = rx2.try_recv().unwrap().unwrap().extract(py).unwrap();
            assert_eq!(r1, "one");
            assert_eq!(r2, "two");
        });
    }

    #[test]
    fn drive_time_budget_triggers() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            // Custom awaitable that yield-Nones with a busy sleep between each,
            // burning wall-clock time to exceed the 1ms time budget.
            py.run(
                c"
import time
class SlowYields:
    def __await__(self):
        for _ in range(10000):
            time.sleep(0.001)  # 1ms per yield — 16 yields = 16ms > budget
            yield None
        return 'done'

async def slow():
    return await SlowYields()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"slow()", None, None).unwrap().unbind();
            let (tx, _rx) = oneshot::channel();
            let mut task = SchedulerTask::new(py, coro, tx).unwrap();
            let time_budget = Duration::from_millis(1);
            let (result, stats) =
                drive_task(py, &mut task, ops.as_ref(), 10_000, time_budget, None);
            assert!(matches!(result, DriveResult::BudgetExhausted));
            assert!(stats.time_budget_exceeded);
            // Should hit time budget well before the 10k step budget.
            assert!((stats.steps as usize) < 10_000);
        });
    }

    #[test]
    fn drive_time_budget_does_not_trigger_fast_coro() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
            py.run(c"async def fast(): return 42", None, None).unwrap();
            let coro = py.eval(c"fast()", None, None).unwrap().unbind();
            let (tx, _rx) = oneshot::channel();
            let mut task = SchedulerTask::new(py, coro, tx).unwrap();
            let (result, stats) =
                drive_task(py, &mut task, ops.as_ref(), 128, DEFAULT_TIME_BUDGET, None);
            assert!(matches!(result, DriveResult::Completed(_)));
            assert!(!stats.time_budget_exceeded);
            assert!(!stats.budget_exhausted);
        });
    }

    #[test]
    fn drive_stats_tracks_time_budget_field() {
        let stats = DriveStats::default();
        assert!(!stats.time_budget_exceeded);
        assert!(!stats.budget_exhausted);

        let stats = DriveStats {
            time_budget_exceeded: true,
            budget_exhausted: true,
            ..DriveStats::default()
        };
        assert!(stats.time_budget_exceeded);
        assert!(stats.budget_exhausted);
    }
}
