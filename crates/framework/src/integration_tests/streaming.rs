//! Integration tests for streaming ASGI responses.
//!
//! Reproduces the `StreamingResponse` + `anyio.create_task_group()` pattern
//! used by Starlette to verify that the Rust scheduler's `_SchedulerTask`
//! properly delegates to `_enter_task`/`_leave_task` so asyncio can step
//! its own tasks without conflict.
//!
//! The production error (now fixed) was:
//! ```text
//! RuntimeError: Cannot enter into task <Task ...wrap...>
//!   while another task <apx._core.TaskProxy ...> is being executed.
//! ```

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyAnyMethods;

use crate::io::bridge::queue::ReadyQueue;
use crate::io::bridge::spawn_and_drive;
use crate::io::driver::ffi::{CoroutineOps, FfiCoroutineOps};
use crate::io::reactor::TaskOps;

/// Shared test harness: set up event loop, asyncio thread, drive a coroutine,
/// poll for result, and clean up.
struct StreamingTestHarness {
    ops: Arc<dyn CoroutineOps>,
    ready_queue: Arc<ReadyQueue>,
    event_loop: Py<PyAny>,
    call_soon_threadsafe: Py<PyAny>,
    task_ops: TaskOps,
    asyncio_thread: Option<std::thread::JoinHandle<()>>,
}

impl StreamingTestHarness {
    /// Set up event loop + asyncio thread. Must be called inside `Python::attach`.
    fn new(py: Python<'_>) -> Self {
        // Ensure `apx._task` is importable in the test environment.
        // We can't just add `src/` to sys.path because `apx/__init__.py`
        // calls `version("apx")` which fails when the package isn't installed.
        // Instead, register a stub `apx` package, then import `_task` from source.
        // The check for `_SchedulerTask` guards against partial initialization
        // when tests run in parallel threads.
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

        let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
        let ready_queue = Arc::new(ReadyQueue::new());

        let asyncio = py.import(c"asyncio").unwrap();
        let event_loop = asyncio.call_method0(c"new_event_loop").unwrap();
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .unwrap();
        let events = py.import(c"asyncio.events").unwrap();
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .unwrap();

        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .unwrap()
            .unbind();

        // Cache task lifecycle callables.
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

    /// Drive a coroutine and return the oneshot receiver. Must be called inside `Python::attach`.
    fn drive(
        &self,
        py: Python<'_>,
        coro: Py<PyAny>,
    ) -> tokio::sync::oneshot::Receiver<Result<Py<PyAny>, crate::protocol::http::error::AppError>>
    {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        spawn_and_drive(
            py,
            coro,
            result_tx,
            &self.ops,
            &self.call_soon_threadsafe,
            &self.ready_queue,
            &self.task_ops,
        );
        result_rx
    }

    /// Poll for result, alternating between GIL release (for asyncio thread)
    /// and drain (for ready queue). Returns the extracted string result.
    fn poll_result(
        &self,
        mut result_rx: tokio::sync::oneshot::Receiver<
            Result<Py<PyAny>, crate::protocol::http::error::AppError>,
        >,
    ) -> Result<String, String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            // Release GIL so the asyncio thread can step tasks.
            std::thread::sleep(Duration::from_millis(10));

            // Acquire GIL, drain ready queue, check result.
            let done = Python::attach(|py| {
                self.ready_queue.drain(
                    py,
                    &self.ops,
                    &self.call_soon_threadsafe,
                    &self.ready_queue,
                    &self.task_ops,
                );
                // Wake the event loop thread. Items added to `_ready`
                // via `call_soon` (e.g. by `loop.create_task()` during a
                // drive cycle) don't wake the selector. A no-op
                // `call_soon_threadsafe` pokes the self-pipe so
                // `_run_once` returns from `select()` and processes them.
                let noop = py.eval(c"lambda: None", None, None).unwrap();
                let _ = self.call_soon_threadsafe.call1(py, (noop,));
                match result_rx.try_recv() {
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

    /// Stop the asyncio loop and join the thread.
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
            let events = py.import(c"asyncio.events").unwrap();
            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let el = self.event_loop.bind(py);
            let _ = el.call_method0(c"close");
        });
    }
}

impl Drop for StreamingTestHarness {
    fn drop(&mut self) {
        if self.asyncio_thread.is_some() {
            self.shutdown();
        }
    }
}

/// A coroutine that creates an asyncio.Task must complete successfully.
/// The `_SchedulerTask` in `_current_tasks` must not block asyncio from stepping
/// the newly created task.
#[test]
fn asyncio_task_created_during_drive_completes() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    // Set up + drive in a single GIL block so _running_loop stays set.
    let (mut harness, result_rx) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        py.run(
            c"
import asyncio

async def inner():
    return 'hello from inner task'

async def app_that_creates_task():
    loop = asyncio.get_running_loop()
    task = loop.create_task(inner())
    await asyncio.sleep(0)
    result = await task
    return result
",
            None,
            None,
        )
        .unwrap();

        let coro = py
            .eval(c"app_that_creates_task()", None, None)
            .unwrap()
            .unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(result_rx);
    harness.shutdown();

    match result {
        Ok(val) => assert!(
            val.contains("hello from inner task"),
            "unexpected result: {val}"
        ),
        Err(err) => panic!("test failed: {err}"),
    }
}

/// Reproduce the Starlette StreamingResponse pattern with concurrent tasks.
/// This is the exact pattern that fails in production with the old TaskProxy:
/// "Cannot enter into task ... while another task TaskProxy is being executed"
#[test]
fn starlette_streaming_response_pattern() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, result_rx) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        py.run(
            c"
import asyncio

async def stream_producer(results):
    for i in range(5):
        results.append(f'chunk-{i}')
        await asyncio.sleep(0)

async def disconnect_listener():
    await asyncio.sleep(0.05)

async def streaming_app():
    results = []
    loop = asyncio.get_running_loop()
    producer_task = loop.create_task(stream_producer(results))
    listener_task = loop.create_task(disconnect_listener())

    await producer_task

    listener_task.cancel()
    try:
        await listener_task
    except asyncio.CancelledError:
        pass

    return ','.join(results)
",
            None,
            None,
        )
        .unwrap();

        let coro = py.eval(c"streaming_app()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(result_rx);
    harness.shutdown();

    match result {
        Ok(val) => assert!(
            val.contains("chunk-0") && val.contains("chunk-4"),
            "unexpected result: {val}"
        ),
        Err(err) => panic!("streaming pattern failed (production bug): {err}"),
    }
}

/// Test the anyio task group pattern — this is the exact pattern Starlette
/// uses internally that triggers `_enter_task`/`_leave_task` conflicts.
///
/// Runs in a **subprocess** for full process isolation. anyio's
/// `create_task_group` interacts with asyncio global module state (sniffio
/// detection, `_task_states`, `_running_loop`) that gets polluted by other
/// tests sharing the same embedded Python interpreter. A fresh process
/// guarantees clean state.
#[test]
fn anyio_task_group_with_scheduler_task() {
    // Re-exec ourselves in a child process with a specific test filter.
    // The child runs `anyio_task_group_impl` (below) with clean Python state.
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .args([
            "integration_tests::streaming::anyio_task_group_impl",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("APX_SUBPROCESS_TEST", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "anyio subprocess test failed (exit={}):\n{stderr}",
        output.status,
    );
}

/// Actual anyio test logic — only executes when invoked as a subprocess.
#[test]
fn anyio_task_group_impl() {
    if std::env::var("APX_SUBPROCESS_TEST").is_err() {
        // Skip when run as part of the main test suite — the outer
        // `anyio_task_group_with_scheduler_task` spawns us in a subprocess.
        return;
    }

    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let has_anyio = Python::attach(|py| py.import(c"anyio").is_ok());
    if !has_anyio {
        eprintln!("anyio not available, skipping");
        return;
    }

    let (mut harness, result_rx) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        py.run(
            c"
import anyio

async def worker(name, results):
    results.append(f'{name}-done')

async def anyio_app():
    results = []
    async with anyio.create_task_group() as tg:
        tg.start_soon(worker, 'a', results)
        tg.start_soon(worker, 'b', results)
    results.sort()
    return ','.join(results)
",
            None,
            None,
        )
        .unwrap();

        let coro = py.eval(c"anyio_app()", None, None).unwrap().unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(result_rx);
    harness.shutdown();

    match result {
        Ok(val) => assert!(
            val.contains("a-done") && val.contains("b-done"),
            "unexpected result: {val}"
        ),
        Err(err) => panic!("anyio task group pattern failed: {err}"),
    }
}

/// Contextvars set in middleware must survive across await boundaries.
/// This is the exact scenario where the old code failed: the drain task
/// re-drives the coroutine and the context is wrong.
#[test]
fn contextvars_survive_suspension() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, result_rx) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        py.run(
            c"
import contextvars, asyncio

request_id = contextvars.ContextVar('request_id', default='unset')
request_id.set('req-abc-123')

async def check_context_after_suspend():
    before = request_id.get()
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    loop.call_soon_threadsafe(fut.set_result, 'woke')
    await fut
    after = request_id.get()
    return f'{before},{after}'
",
            None,
            None,
        )
        .unwrap();

        let coro = py
            .eval(c"check_context_after_suspend()", None, None)
            .unwrap()
            .unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    let result = harness.poll_result(result_rx);
    harness.shutdown();

    match result {
        Ok(val) => assert_eq!(
            val, "req-abc-123,req-abc-123",
            "contextvar must survive suspension, got: {val}"
        ),
        Err(err) => panic!("contextvars_survive_suspension failed: {err}"),
    }
}

/// Two requests with different contextvars must not see each other's values.
#[test]
fn contextvars_isolated_between_requests() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, rx1, rx2) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        py.run(
            c"
import contextvars, asyncio

req_var = contextvars.ContextVar('req_var', default='none')

async def handler(tag):
    req_var.set(tag)
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    loop.call_soon_threadsafe(fut.set_result, 'done')
    await fut
    return f'{tag}={req_var.get()}'
",
            None,
            None,
        )
        .unwrap();

        let coro1 = py.eval(c"handler('A')", None, None).unwrap().unbind();
        let coro2 = py.eval(c"handler('B')", None, None).unwrap().unbind();
        let rx1 = harness.drive(py, coro1);
        let rx2 = harness.drive(py, coro2);
        (harness, rx1, rx2)
    });

    let r1 = harness.poll_result(rx1);
    let r2 = harness.poll_result(rx2);
    harness.shutdown();

    match (&r1, &r2) {
        (Ok(v1), Ok(v2)) => {
            assert!(v1.contains("A=A"), "request 1 got: {v1}");
            assert!(v2.contains("B=B"), "request 2 got: {v2}");
        }
        _ => panic!("isolation test failed: r1={r1:?}, r2={r2:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bug reproduction: Future.with_channel() wakers not fired on resolve
// ---------------------------------------------------------------------------

/// Verifies that `Future::pending()` + `Future::set_result()` fires
/// done-callbacks immediately, allowing the Rust scheduler to resume the
/// suspended coroutine. This is the fixed code path for ASGI backpressure.
///
/// Previously, `Future::with_channel()` + `resolve_tx.send()` was used,
/// which only deposited the value in the oneshot channel without firing
/// wakers — causing all streaming requests to hang (stream_10 = 0 req/s).
#[test]
fn rust_future_channel_resolve_fires_wakers() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, mut result_rx) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        // Create a pending Future (the fixed pattern used by ASGI backpressure).
        let py_future = Py::new(py, crate::io::driver::primitives::Future::pending()).unwrap();
        let fut_ref = py_future.clone_ref(py);

        // Resolve the future from a background thread after a short delay.
        // This mirrors the fixed backpressure tokio task:
        //   handle.spawn(async move {
        //       tx.send(event).await;
        //       Python::attach(|py| { Future::set_result(fut_ref, py, py.None()); });
        //   });
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            Python::attach(|py| {
                let _ = crate::io::driver::primitives::Future::set_result(fut_ref, py, py.None());
            });
        });

        // Coroutine that awaits the Rust Future.
        let builtins = py.import(c"builtins").unwrap();
        builtins
            .setattr(c"_test_rust_future", py_future.bind(py))
            .unwrap();
        py.run(
            c"
import builtins
async def _await_rust_future():
    await builtins._test_rust_future
    return 'backpressure_resolved'
",
            None,
            None,
        )
        .unwrap();

        let coro = py
            .eval(c"_await_rust_future()", None, None)
            .unwrap()
            .unbind();
        let rx = harness.drive(py, coro);
        (harness, rx)
    });

    // Use a short timeout — before the fix, this would hang forever.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut result = Err("timed out".to_owned());
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        let done = Python::attach(|py| {
            harness.ready_queue.drain(
                py,
                &harness.ops,
                &harness.call_soon_threadsafe,
                &harness.ready_queue,
                &harness.task_ops,
            );
            match result_rx.try_recv() {
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
        if let Some(r) = done {
            result = r;
            break;
        }
    }
    harness.shutdown();

    match result {
        Ok(val) => assert_eq!(val, "backpressure_resolved", "unexpected result: {val}"),
        Err(err) => panic!(
            "rust_future_channel_resolve_fires_wakers FAILED: {err}\n\
             Future::set_result() should fire done-callbacks immediately, \
             resuming the suspended coroutine."
        ),
    }
}

// ---------------------------------------------------------------------------
// Bug reproduction: _SchedulerTask sentinel conflicts with _enter_task
// ---------------------------------------------------------------------------

/// Verifies that the immediately-completing sentinel in `_SchedulerTask` does
/// NOT produce "_enter_task" conflicts. With the old forever-suspended sentinel,
/// `__step` would call `_enter_task(loop, task)` and yield, leaving the task
/// "entered" — racing with the Rust driver's own `_enter_task` call.
///
/// With the fixed sentinel (empty body), `__step` enters → sentinel returns →
/// `__step` leaves, all atomically in one synchronous callback. The race window
/// collapses. Additionally, `self.cancel()` in `__init__` prevents the task
/// from lingering in `asyncio.all_tasks()`.
#[test]
fn scheduler_task_sentinel_conflicts_with_enter_task() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errors_clone = Arc::clone(&errors);

    // Phase 1: set up harness, install error capture, create task, enter it.
    let (mut harness, loop_obj, sched_task) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        // Install a custom exception handler that captures errors.
        py.run(
            c"
import builtins
builtins._sentinel_errors = []
def _capture_handler(loop, context):
    msg = context.get('message', '')
    exc = context.get('exception')
    if exc:
        msg = f'{msg}: {exc}'
    builtins._sentinel_errors.append(msg)
",
            None,
            None,
        )
        .unwrap();
        let handler = py.eval(c"_capture_handler", None, None).unwrap();
        harness
            .event_loop
            .call_method1(py, c"set_exception_handler", (handler,))
            .unwrap();

        // Create a _SchedulerTask — this schedules call_soon(self.__step).
        let asyncio = py.import(c"asyncio").unwrap();
        let loop_obj = asyncio.call_method0(c"get_running_loop").unwrap().unbind();
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("loop", loop_obj.bind(py)).unwrap();
        let dummy_coro = py.eval(c"(lambda: None)()", None, None).unwrap().unbind();
        let sched_task = harness
            .task_ops
            .scheduler_task_cls
            .call(py, (dummy_coro,), Some(&kwargs))
            .unwrap();

        // Enter the task as "current" — just like spawn_and_drive does.
        harness
            .task_ops
            .enter_task
            .call1(py, (&loop_obj, &sched_task))
            .unwrap();

        (harness, loop_obj, sched_task)
    });

    // Phase 2: GIL released — asyncio thread processes the sentinel __step.
    // Since we removed _ready cancellation (step 2), the sentinel __step WILL
    // try _enter_task while we're holding it, producing a "Cannot enter into
    // task" error. This is expected — in production, spawn_and_drive
    // enters/drives/leaves before the asyncio thread processes __step.
    std::thread::sleep(Duration::from_millis(200));

    // Phase 3: reacquire GIL, leave task, collect errors.
    Python::attach(|py| {
        let _ = harness
            .task_ops
            .leave_task
            .call1(py, (&loop_obj, &sched_task));
    });

    // Give asyncio thread time to process any remaining callbacks.
    std::thread::sleep(Duration::from_millis(100));

    Python::attach(|py| {
        let captured: Vec<String> = py
            .eval(c"builtins._sentinel_errors", None, None)
            .unwrap()
            .extract()
            .unwrap();
        let mut errs = errors_clone.lock().unwrap();
        errs.extend(captured);
    });

    harness.shutdown();

    // The sentinel __step conflict is expected when manually holding
    // _enter_task across a GIL release. The production flow (spawn_and_drive)
    // avoids this because it enters/drives/leaves synchronously before the
    // asyncio thread processes the scheduled __step callback.
    let errs = errors.lock().unwrap();
    let has_enter_conflict = errs.iter().any(|e| e.contains("Cannot enter into task"));
    assert!(
        has_enter_conflict,
        "Expected 'Cannot enter into task' conflict from sentinel __step \
         (sentinel no longer cancelled via _ready), but got none.",
    );
}

// ---------------------------------------------------------------------------
// Bug reproduction: /stream/10 endpoint pattern — async generator + sleep(0)
// ---------------------------------------------------------------------------

/// Reproduces the exact `/stream/{chunks}` endpoint pattern from the bench app.
/// An async generator yields chunks with `await asyncio.sleep(0)` between them.
/// Multiple concurrent requests trigger "Cannot enter into task" errors when
/// the sentinel `__step` conflicts with `async_generator_athrow` cleanup tasks.
#[test]
fn stream_endpoint_async_generator_pattern() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errors_clone = Arc::clone(&errors);

    let (mut harness, rx1, rx2, rx3) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

        // Install error capture.
        py.run(
            c"
import builtins
builtins._stream_errors = []
def _stream_capture(loop, context):
    msg = context.get('message', '')
    exc = context.get('exception')
    if exc:
        msg = f'{msg}: {exc}'
    builtins._stream_errors.append(msg)
",
            None,
            None,
        )
        .unwrap();
        let handler = py.eval(c"_stream_capture", None, None).unwrap();
        harness
            .event_loop
            .call_method1(py, c"set_exception_handler", (handler,))
            .unwrap();

        // Exact pattern from scripts/bench/app/api.py stream_response endpoint.
        py.run(
            c"
import asyncio

async def generate(chunks):
    for i in range(chunks):
        yield f'chunk-{i}\\n'
        await asyncio.sleep(0)

async def stream_handler():
    # Iterate the async generator (like StreamingResponse.__call__ does).
    result = []
    async for chunk in generate(10):
        result.append(chunk)
    return ''.join(result)
",
            None,
            None,
        )
        .unwrap();

        // Drive 3 concurrent requests — the conflict needs concurrency.
        let c1 = py.eval(c"stream_handler()", None, None).unwrap().unbind();
        let c2 = py.eval(c"stream_handler()", None, None).unwrap().unbind();
        let c3 = py.eval(c"stream_handler()", None, None).unwrap().unbind();
        let rx1 = harness.drive(py, c1);
        let rx2 = harness.drive(py, c2);
        let rx3 = harness.drive(py, c3);
        (harness, rx1, rx2, rx3)
    });

    let r1 = harness.poll_result(rx1);
    let r2 = harness.poll_result(rx2);
    let r3 = harness.poll_result(rx3);

    // Collect errors before shutdown.
    Python::attach(|py| {
        let captured: Vec<String> = py
            .eval(c"builtins._stream_errors", None, None)
            .unwrap()
            .extract()
            .unwrap();
        let mut errs = errors_clone.lock().unwrap();
        errs.extend(captured);
    });

    harness.shutdown();

    // Check for the production failure pattern.
    let errs = errors.lock().unwrap();
    let enter_errors: Vec<_> = errs
        .iter()
        .filter(|e| e.contains("Cannot enter into task"))
        .collect();
    assert!(
        enter_errors.is_empty(),
        "stream_10 pattern produced {n} 'Cannot enter into task' errors:\n{errors:#?}\n\n\
         This is the production bug from /stream/10 endpoint.",
        n = enter_errors.len(),
        errors = enter_errors,
    );

    // All 3 requests should complete successfully.
    assert!(r1.is_ok(), "request 1 failed: {r1:?}");
    assert!(r2.is_ok(), "request 2 failed: {r2:?}");
    assert!(r3.is_ok(), "request 3 failed: {r3:?}");

    let v1 = r1.unwrap();
    assert!(
        v1.contains("chunk-0") && v1.contains("chunk-9"),
        "unexpected r1: {v1}"
    );
}

/// Same pattern as above but on uvloop — this is the exact production config.
/// Runs in a subprocess for clean uvloop state.
#[test]
fn stream_endpoint_uvloop_subprocess() {
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new(exe)
        .args([
            "integration_tests::streaming::stream_endpoint_uvloop_impl",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("APX_SUBPROCESS_TEST", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stream uvloop subprocess test failed (exit={}):\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );
}

#[test]
fn stream_endpoint_uvloop_impl() {
    if std::env::var("APX_SUBPROCESS_TEST").is_err() {
        return;
    }

    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let has_uvloop = Python::attach(|py| py.import(c"uvloop").is_ok());
    if !has_uvloop {
        eprintln!("uvloop not available, skipping");
        return;
    }

    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errors_clone = Arc::clone(&errors);

    // Build harness with uvloop.
    let mut harness = Python::attach(|py| {
        // Import apx._task
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

        let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());
        let ready_queue = Arc::new(ReadyQueue::new());

        // Create uvloop event loop.
        let uvloop = py.import(c"uvloop").unwrap();
        let event_loop = uvloop.call_method0(c"new_event_loop").unwrap();
        let asyncio = py.import(c"asyncio").unwrap();
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .unwrap();
        let events = py.import(c"asyncio.events").unwrap();
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .unwrap();

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

        // Error capture.
        py.run(
            c"
import builtins
builtins._uv_errors = []
def _uv_capture(loop, context):
    msg = context.get('message', '')
    exc = context.get('exception')
    if exc:
        msg = f'{msg}: {exc}'
    builtins._uv_errors.append(msg)
",
            None,
            None,
        )
        .unwrap();
        let handler = py.eval(c"_uv_capture", None, None).unwrap();
        event_loop
            .call_method1(c"set_exception_handler", (handler,))
            .unwrap();

        // Async generator + sleep(0) pattern.
        py.run(
            c"
import asyncio

async def generate(chunks):
    for i in range(chunks):
        yield f'chunk-{i}\\n'
        await asyncio.sleep(0)

async def stream_handler():
    result = []
    async for chunk in generate(10):
        result.append(chunk)
    return ''.join(result)
",
            None,
            None,
        )
        .unwrap();

        // Start asyncio thread.
        let el_for_thread = event_loop.clone().unbind();
        let asyncio_thread = std::thread::Builder::new()
            .name("test-uvloop".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    let _ = el.call_method0(c"run_forever");
                });
            })
            .unwrap();

        StreamingTestHarness {
            ops,
            ready_queue,
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            task_ops,
            asyncio_thread: Some(asyncio_thread),
        }
    });

    // Drive 50 requests in batches of 10, releasing the GIL between batches
    // so the uvloop thread processes sentinel __step callbacks while new
    // requests arrive — matching production load pattern.
    for _batch in 0..5 {
        Python::attach(|py| {
            for _ in 0..10 {
                let coro = py.eval(c"stream_handler()", None, None).unwrap().unbind();
                let (tx, _) = tokio::sync::oneshot::channel();
                spawn_and_drive(
                    py,
                    coro,
                    tx,
                    &harness.ops,
                    &harness.call_soon_threadsafe,
                    &harness.ready_queue,
                    &harness.task_ops,
                );
            }
            harness.ready_queue.drain(
                py,
                &harness.ops,
                &harness.call_soon_threadsafe,
                &harness.ready_queue,
                &harness.task_ops,
            );
        });
        // Release GIL — uvloop thread processes sentinels + async generator cleanup.
        std::thread::sleep(Duration::from_millis(50));
    }

    // Extra settle time for uvloop to process remaining callbacks.
    std::thread::sleep(Duration::from_millis(200));

    // Collect errors.
    Python::attach(|py| {
        harness.ready_queue.drain(
            py,
            &harness.ops,
            &harness.call_soon_threadsafe,
            &harness.ready_queue,
            &harness.task_ops,
        );
        let captured: Vec<String> = py
            .eval(c"builtins._uv_errors", None, None)
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

    // Print diagnostic info.
    if !enter_errors.is_empty() {
        eprintln!(
            "EXPECTED FAILURE: {n} 'Cannot enter into task' errors on uvloop:\n{errors:#?}",
            n = enter_errors.len(),
            errors = enter_errors,
        );
    }

    assert!(
        enter_errors.is_empty(),
        "stream_10 uvloop pattern produced {n} 'Cannot enter into task' errors:\n{errors:#?}\n\n\
         This is the production bug from /stream/10 on uvloop.",
        n = enter_errors.len(),
        errors = enter_errors,
    );
}
