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

use crate::ffi::{CoroutineOps, FfiCoroutineOps};
use crate::scheduler::driver::{TaskOps, spawn_and_drive};
use crate::scheduler::queue::ReadyQueue;

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
#[test]
fn anyio_task_group_with_scheduler_task() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    // Check if anyio is available, skip if not.
    let has_anyio = Python::attach(|py| py.import(c"anyio").is_ok());
    if !has_anyio {
        eprintln!("anyio not available, skipping anyio_task_group_with_scheduler_task");
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
