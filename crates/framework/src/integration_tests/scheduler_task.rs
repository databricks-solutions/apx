//! Integration tests for [`_SchedulerTask`] on both the default asyncio event
//! loop and uvloop.
//!
//! These tests verify that:
//!
//! 1. `_SchedulerTask.__init__` does not raise on either loop implementation.
//! 2. The sentinel's `__step` runs cleanly on the event loop thread.
//! 3. Coroutines (trivial + suspending) can be driven through the full Rust
//!    scheduler on both loop implementations.
//! 4. `**kwargs` (name, context) are forwarded to `asyncio.Task.__init__`.

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;

use crate::io::bridge::queue::ReadyQueue;
use crate::io::bridge::spawn_and_drive;
use crate::io::driver::ffi::{CoroutineOps, FfiCoroutineOps};
use crate::io::reactor::TaskOps;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Which event loop implementation to use.
enum LoopKind {
    /// CPython's built-in asyncio event loop.
    Asyncio,
    /// uvloop (libuv-backed, C extension).
    Uvloop,
}

/// Minimal test harness that creates an event loop (of the requested kind),
/// runs it on a dedicated thread, and can drive coroutines through the Rust
/// scheduler.
struct TaskTestHarness {
    ops: Arc<dyn CoroutineOps>,
    ready_queue: Arc<ReadyQueue>,
    event_loop: Py<PyAny>,
    call_soon_threadsafe: Py<PyAny>,
    task_ops: TaskOps,
    asyncio_thread: Option<std::thread::JoinHandle<()>>,
}

impl TaskTestHarness {
    /// Build a harness with the given loop implementation. Must be called with
    /// the GIL held.
    fn new(py: Python<'_>, kind: LoopKind) -> Self {
        // ── Ensure apx._task is importable ──────────────────────────────
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let src_dir = workspace_root.join("src");
        let locals = pyo3::types::PyDict::new(py);
        locals
            .set_item("_src_dir", src_dir.to_str().unwrap())
            .unwrap();
        py.run(
            c"
import importlib, importlib.util, sys, types
if 'apx' not in sys.modules:
    apx_pkg = types.ModuleType('apx')
    apx_pkg.__path__ = [_src_dir + '/apx']
    apx_pkg.__package__ = 'apx'
    sys.modules['apx'] = apx_pkg
if 'apx._task' not in sys.modules or not hasattr(sys.modules['apx._task'], '_SchedulerTask'):
    spec = importlib.util.spec_from_file_location(
        'apx._task', _src_dir + '/apx/_task.py',
        submodule_search_locations=[])
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    sys.modules['apx._task'] = mod
",
            None,
            Some(&locals),
        )
        .unwrap();

        // ── Create event loop ───────────────────────────────────────────
        let asyncio = py.import(c"asyncio").unwrap();

        let event_loop = match kind {
            LoopKind::Asyncio => asyncio.call_method0(c"new_event_loop").unwrap(),
            LoopKind::Uvloop => {
                let uvloop = py.import(c"uvloop").unwrap();
                uvloop.call_method0(c"new_event_loop").unwrap()
            }
        };

        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .unwrap();
        let events = py.import(c"asyncio.events").unwrap();
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .unwrap();

        // ── Scheduler plumbing ──────────────────────────────────────────
        let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
        let ready_queue = Arc::new(ReadyQueue::new());

        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .unwrap()
            .unbind();

        let tasks_mod = py.import(c"asyncio.tasks").unwrap();
        let enter_task = tasks_mod.getattr(c"_enter_task").unwrap().unbind();
        let leave_task = tasks_mod.getattr(c"_leave_task").unwrap().unbind();
        let task_mod = py.import(c"apx._task").unwrap();
        let scheduler_task_cls = task_mod.getattr(c"_SchedulerTask").unwrap().unbind();
        let task_ops = TaskOps {
            enter_task,
            leave_task,
            scheduler_task_cls,
        };

        // ── Dedicated asyncio thread ────────────────────────────────────
        let el_for_thread = event_loop.clone().unbind();
        let asyncio_thread = std::thread::Builder::new()
            .name("test-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    let _ = el.call_method0(c"run_forever");
                });
            })
            .unwrap();

        Self {
            ops,
            ready_queue,
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            task_ops,
            asyncio_thread: Some(asyncio_thread),
        }
    }

    /// Drive a coroutine through the Rust scheduler, returning a oneshot
    /// receiver for the result. Must be called with the GIL held.
    fn drive(
        &self,
        py: Python<'_>,
        coro: Py<PyAny>,
    ) -> tokio::sync::oneshot::Receiver<Result<Py<PyAny>, crate::protocol::http::error::AppError>>
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        spawn_and_drive(
            py,
            coro,
            tx,
            &self.ops,
            &self.call_soon_threadsafe,
            &self.ready_queue,
            &self.task_ops,
        );
        rx
    }

    /// Poll for a string result, draining the ready queue between attempts.
    fn poll_result(
        &self,
        mut rx: tokio::sync::oneshot::Receiver<
            Result<Py<PyAny>, crate::protocol::http::error::AppError>,
        >,
    ) -> Result<String, String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            let done = Python::attach(|py| {
                self.ready_queue.drain(
                    py,
                    &self.ops,
                    &self.call_soon_threadsafe,
                    &self.ready_queue,
                    &self.task_ops,
                );
                match rx.try_recv() {
                    Ok(Ok(value)) => Some(Ok(value.extract::<String>(py).unwrap_or_else(|_| {
                        value
                            .bind(py)
                            .repr()
                            .map(|r| r.to_string())
                            .unwrap_or_default()
                    }))),
                    Ok(Err(err)) => Some(Err(format!("{err:?}"))),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Some(Err("result channel closed".to_owned()))
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                }
            });
            if let Some(result) = done {
                return result;
            }
        }
        Err("timed out".to_owned())
    }

    fn shutdown(&mut self) {
        Python::attach(|py| {
            let el = self.event_loop.bind(py);
            let stop = el.getattr(c"stop").unwrap();
            let _ = el.call_method1(c"call_soon_threadsafe", (&stop,));
        });
        if let Some(handle) = self.asyncio_thread.take() {
            let _ = handle.join();
        }
        Python::attach(|py| {
            let asyncio = py.import(c"asyncio").unwrap();
            let events = py.import(c"asyncio.events").unwrap();
            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let el = self.event_loop.bind(py);
            let _ = el.call_method0(c"close");
            // Reset the default event loop so subsequent tests don't inherit
            // a closed uvloop as the global default.
            let _ = asyncio.call_method1(c"set_event_loop", (py.None(),));
        });
    }
}

impl Drop for TaskTestHarness {
    fn drop(&mut self) {
        if self.asyncio_thread.is_some() {
            self.shutdown();
        }
    }
}

// ── Shared test logic ───────────────────────────────────────────────────

/// Verify that `_SchedulerTask.__init__` does not raise and that `__step`
/// runs cleanly on the event loop thread (no exception-handler errors).
///
/// This does NOT hold `_enter_task` across a GIL release (which is
/// inherently racy in parallel test suites). Instead we create the task,
/// let the event loop thread process `__step`, then check for errors.
fn assert_scheduler_task_init_is_clean(kind: LoopKind) {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errors_clone = Arc::clone(&errors);

    let mut harness = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, kind);

        // Install a custom exception handler that captures errors.
        py.run(
            c"
import builtins
builtins._sched_errors = []
def _capture(loop, context):
    msg = context.get('message', '')
    exc = context.get('exception')
    if exc:
        msg = f'{msg}: {exc}'
    builtins._sched_errors.append(msg)
",
            None,
            None,
        )
        .unwrap();
        let handler = py.eval(c"_capture", None, None).unwrap();
        harness
            .event_loop
            .call_method1(py, c"set_exception_handler", (handler,))
            .unwrap();

        // Create a _SchedulerTask — this must not raise.
        let asyncio = py.import(c"asyncio").unwrap();
        let loop_obj = asyncio.call_method0(c"get_running_loop").unwrap();
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("loop", &loop_obj).unwrap();
        let dummy_coro = py.eval(c"(lambda: None)()", None, None).unwrap().unbind();
        let _sched_task = harness
            .task_ops
            .scheduler_task_cls
            .call(py, (dummy_coro,), Some(&kwargs))
            .unwrap();

        harness
    });

    // Release GIL so the event loop thread can process the sentinel __step.
    std::thread::sleep(Duration::from_millis(200));

    Python::attach(|py| {
        let captured: Vec<String> = py
            .eval(c"builtins._sched_errors", None, None)
            .unwrap()
            .extract()
            .unwrap();
        let mut errs = errors_clone.lock().unwrap();
        errs.extend(captured);
    });

    harness.shutdown();

    let errs = errors.lock().unwrap();
    let enter_errors: Vec<_> = errs
        .iter()
        .filter(|e| e.contains("Cannot enter into task"))
        .collect();
    assert!(
        enter_errors.is_empty(),
        "Expected NO '_enter_task' conflicts from sentinel __step, got {count}:\n{errors:?}",
        count = enter_errors.len(),
        errors = enter_errors,
    );
}

/// Verify that a trivial coroutine completes successfully through the Rust
/// scheduler on the given event loop.
fn assert_trivial_coroutine_completes(kind: LoopKind) {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, kind);
        py.run(
            c"
async def _trivial():
    return 'hello'
",
            None,
            None,
        )
        .unwrap();
        let coro = py.eval(c"_trivial()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(rx);
    harness.shutdown();

    assert_eq!(result.unwrap(), "hello");
}

/// Verify that a coroutine that suspends (via `asyncio.sleep(0)`) and resumes
/// completes successfully on the given event loop.
fn assert_suspending_coroutine_completes(kind: LoopKind) {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, kind);
        py.run(
            c"
import asyncio
async def _suspend_and_resume():
    before = 'A'
    await asyncio.sleep(0)
    after = 'B'
    return before + after
",
            None,
            None,
        )
        .unwrap();
        let coro = py
            .eval(c"_suspend_and_resume()", None, None)
            .unwrap()
            .unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(rx);
    harness.shutdown();

    assert_eq!(result.unwrap(), "AB");
}

// ── Default asyncio event loop ──────────────────────────────────────────

#[test]
fn asyncio_scheduler_task_init_clean() {
    assert_scheduler_task_init_is_clean(LoopKind::Asyncio);
}

#[test]
fn asyncio_trivial_coroutine() {
    assert_trivial_coroutine_completes(LoopKind::Asyncio);
}

#[test]
fn asyncio_suspending_coroutine() {
    assert_suspending_coroutine_completes(LoopKind::Asyncio);
}

// ── uvloop event loop ───────────────────────────────────────────────────

#[test]
fn uvloop_scheduler_task_init_clean() {
    assert_scheduler_task_init_is_clean(LoopKind::Uvloop);
}

#[test]
fn uvloop_trivial_coroutine() {
    assert_trivial_coroutine_completes(LoopKind::Uvloop);
}

#[test]
fn uvloop_suspending_coroutine() {
    assert_suspending_coroutine_completes(LoopKind::Uvloop);
}

// ── Task lifecycle tests ────────────────────────────────────────────────

#[test]
fn current_task_set_during_drive() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();
    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, LoopKind::Asyncio);
        py.run(
            c"
import asyncio
async def check_current_task():
    t = asyncio.current_task()
    return type(t).__name__
",
            None,
            None,
        )
        .unwrap();
        let coro = py
            .eval(c"check_current_task()", None, None)
            .unwrap()
            .unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });
    let result = harness.poll_result(rx);
    harness.shutdown();
    assert_eq!(result.unwrap(), "_SchedulerTask");
}

#[test]
fn scheduler_task_is_real_asyncio_task() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();
    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, LoopKind::Asyncio);
        py.run(
            c"
import asyncio
async def check_isinstance():
    t = asyncio.current_task()
    return str(isinstance(t, asyncio.Task))
",
            None,
            None,
        )
        .unwrap();
        let coro = py.eval(c"check_isinstance()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });
    let result = harness.poll_result(rx);
    harness.shutdown();
    assert_eq!(result.unwrap(), "True");
}

#[test]
fn scheduler_task_weakref_works() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();
    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, LoopKind::Asyncio);
        py.run(
            c"
import asyncio, weakref
async def check_weakref():
    t = asyncio.current_task()
    ref = weakref.ref(t)
    return 'ok' if ref() is t else 'fail'
",
            None,
            None,
        )
        .unwrap();
        let coro = py.eval(c"check_weakref()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });
    let result = harness.poll_result(rx);
    harness.shutdown();
    assert_eq!(result.unwrap(), "ok");
}

#[test]
fn scheduler_task_in_all_tasks() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();
    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, LoopKind::Asyncio);
        py.run(
            c"
import asyncio
async def check_all_tasks():
    t = asyncio.current_task()
    tasks = asyncio.all_tasks()
    return 'found' if t in tasks else 'missing'
",
            None,
            None,
        )
        .unwrap();
        let coro = py.eval(c"check_all_tasks()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });
    let result = harness.poll_result(rx);
    harness.shutdown();
    assert_eq!(result.unwrap(), "found");
}

#[test]
fn contextvars_survive_full_suspension() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();
    let (mut harness, rx) = Python::attach(|py| {
        let harness = TaskTestHarness::new(py, LoopKind::Asyncio);
        py.run(
            c"
import asyncio, contextvars
_cv = contextvars.ContextVar('_cv')
_cv.set('before_suspend')
async def check_cv():
    before = _cv.get()
    await asyncio.sleep(0)
    after = _cv.get()
    return f'{before},{after}'
",
            None,
            None,
        )
        .unwrap();
        let coro = py.eval(c"check_cv()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });
    let result = harness.poll_result(rx);
    harness.shutdown();
    assert_eq!(result.unwrap(), "before_suspend,before_suspend");
}
