//! Integration tests for streaming ASGI responses with asyncio delegation.
//!
//! Reproduces patterns like `StreamingResponse` + `anyio.create_task_group()`
//! used by Starlette. These tests verify that coroutines submitted via
//! `call_soon_threadsafe(create_task, coro)` behave correctly under asyncio.

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;

/// Shared test harness: set up event loop, asyncio thread, submit a
/// coroutine, poll for result, and clean up.
struct StreamingTestHarness {
    event_loop: Py<PyAny>,
    call_soon_threadsafe: Py<PyAny>,
    create_task: Py<PyAny>,
    asyncio_thread: Option<std::thread::JoinHandle<()>>,
}

impl StreamingTestHarness {
    fn new(py: Python<'_>) -> Self {
        let asyncio = py.import(c"asyncio").expect("import asyncio");
        let event_loop = asyncio
            .call_method0(c"new_event_loop")
            .expect("new_event_loop");
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .expect("set_event_loop");
        let events = py.import(c"asyncio.events").expect("import asyncio.events");
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .expect("_set_running_loop");

        // Enable eager task factory (Python 3.12+).
        if let Ok(eager_factory) = asyncio.getattr(c"eager_task_factory") {
            let _ = event_loop.call_method1(c"set_task_factory", (eager_factory,));
        }

        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .expect("call_soon_threadsafe")
            .unbind();
        let create_task = event_loop
            .getattr(c"create_task")
            .expect("create_task")
            .unbind();

        let el_for_thread = event_loop.clone().unbind();
        let asyncio_thread = std::thread::Builder::new()
            .name("test-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    let _ = el.call_method0(c"run_forever");
                });
            })
            .expect("spawn asyncio thread");

        Self {
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            create_task,
            asyncio_thread: Some(asyncio_thread),
        }
    }

    /// Submit a coroutine and return a result list to poll.
    ///
    /// Wraps the coroutine in a helper that deposits `('ok', result)` or
    /// `('err', str(exc))` into a Python list, which can be polled from Rust.
    fn submit(&self, py: Python<'_>, coro: Py<PyAny>) -> Py<pyo3::types::PyList> {
        let results = pyo3::types::PyList::empty(py).unbind();
        let wrapper_code = py
            .eval(
                c"__builtins__.__import__('builtins').__dict__.get('_apx_test_wrapper', None)",
                None,
                None,
            )
            .expect("check wrapper");

        if wrapper_code.is_none() {
            py.run(
                c"
import builtins

async def _apx_test_wrapper(coro, results):
    try:
        r = await coro
        results.append(('ok', r))
    except BaseException as e:
        results.append(('err', str(e)))

builtins._apx_test_wrapper = _apx_test_wrapper
",
                None,
                None,
            )
            .expect("register wrapper");
        }

        let wrapper_fn = py
            .eval(c"builtins._apx_test_wrapper", None, None)
            .expect("get wrapper");
        let wrapper_coro = wrapper_fn
            .call1((&coro, results.bind(py)))
            .expect("call wrapper");
        self.call_soon_threadsafe
            .call1(py, (&self.create_task, &wrapper_coro))
            .expect("submit to asyncio");
        results
    }

    /// Poll for a result, blocking up to `timeout`.
    fn poll_result(results: &Py<pyo3::types::PyList>, timeout: Duration) -> Result<String, String> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            let done = Python::attach(|py| {
                let list = results.bind(py);
                if list.is_empty() {
                    return None;
                }
                let item = list.get_item(0).expect("get first item");
                let tag: String = item
                    .get_item(0)
                    .expect("get tag")
                    .extract()
                    .expect("extract tag");
                let value: String =
                    item.get_item(1)
                        .and_then(|v| v.extract())
                        .unwrap_or_else(|_| {
                            item.get_item(1)
                                .map(|v| v.repr().map(|r| r.to_string()).unwrap_or_default())
                                .unwrap_or_default()
                        });
                if tag == "ok" {
                    Some(Ok(value))
                } else {
                    Some(Err(value))
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
            if let Ok(stop) = el.getattr(c"stop") {
                let _ = el.call_method1(c"call_soon_threadsafe", (&stop,));
            }
        });
        if let Some(handle) = self.asyncio_thread.take() {
            let _ = handle.join();
        }
        Python::attach(|py| {
            let events = py.import(c"asyncio.events").expect("import asyncio.events");
            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let el = self.event_loop.bind(py);
            let _ = el.call_method0(c"close");
        });
    }
}

impl Drop for StreamingTestHarness {
    fn drop(&mut self) {
        if self.asyncio_thread.is_some() && !std::thread::panicking() {
            self.shutdown();
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

const POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// A coroutine that creates an asyncio.Task must complete successfully.
#[test]
fn asyncio_task_created_during_drive_completes() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, results) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);
        py.run(
            c"
import asyncio

async def inner():
    return 'hello from inner task'

async def app_that_creates_task():
    task = asyncio.get_running_loop().create_task(inner())
    await asyncio.sleep(0)
    return await task
",
            None,
            None,
        )
        .expect("define app");

        let coro = py
            .eval(c"app_that_creates_task()", None, None)
            .expect("create coro")
            .unbind();
        let results = harness.submit(py, coro);
        (harness, results)
    });

    let result = StreamingTestHarness::poll_result(&results, POLL_TIMEOUT);
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
#[test]
fn starlette_streaming_response_pattern() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, results) = Python::attach(|py| {
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
        .expect("define app");

        let coro = py
            .eval(c"streaming_app()", None, None)
            .expect("create coro")
            .unbind();
        let results = harness.submit(py, coro);
        (harness, results)
    });

    let result = StreamingTestHarness::poll_result(&results, POLL_TIMEOUT);
    harness.shutdown();

    match result {
        Ok(val) => assert!(
            val.contains("chunk-0") && val.contains("chunk-4"),
            "unexpected result: {val}"
        ),
        Err(err) => panic!("streaming pattern failed: {err}"),
    }
}

/// Test the anyio task group pattern — this is the exact pattern Starlette
/// uses internally for concurrent async work.
///
/// Runs in a subprocess for full process isolation.
#[test]
fn anyio_task_group_pattern() {
    let exe = std::env::current_exe().expect("current exe");
    let output = std::process::Command::new(exe)
        .args([
            "integration_tests::streaming::anyio_task_group_impl",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("APX_SUBPROCESS_TEST", "1")
        .output()
        .expect("spawn subprocess");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "anyio subprocess test failed (exit={}):\n{stderr}",
        output.status,
    );
}

#[test]
fn anyio_task_group_impl() {
    if std::env::var("APX_SUBPROCESS_TEST").is_err() {
        return;
    }

    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let has_anyio = Python::attach(|py| py.import(c"anyio").is_ok());
    if !has_anyio {
        eprintln!("anyio not available, skipping");
        return;
    }

    let (mut harness, results) = Python::attach(|py| {
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
        .expect("define app");

        let coro = py
            .eval(c"anyio_app()", None, None)
            .expect("create coro")
            .unbind();
        let results = harness.submit(py, coro);
        (harness, results)
    });

    let result = StreamingTestHarness::poll_result(&results, POLL_TIMEOUT);
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
#[test]
fn contextvars_survive_suspension() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let (mut harness, results) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);
        py.run(
            c"
import contextvars, asyncio

request_id = contextvars.ContextVar('request_id', default='unset')
request_id.set('req-abc-123')

async def check_context_after_suspend():
    before = request_id.get()
    fut = asyncio.get_running_loop().create_future()
    asyncio.get_running_loop().call_soon_threadsafe(fut.set_result, 'woke')
    await fut
    after = request_id.get()
    return f'{before},{after}'
",
            None,
            None,
        )
        .expect("define app");

        let coro = py
            .eval(c"check_context_after_suspend()", None, None)
            .expect("create coro")
            .unbind();
        let results = harness.submit(py, coro);
        (harness, results)
    });

    let result = StreamingTestHarness::poll_result(&results, POLL_TIMEOUT);
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

    let (mut harness, r1, r2) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);
        py.run(
            c"
import contextvars, asyncio

req_var = contextvars.ContextVar('req_var', default='none')

async def handler(tag):
    req_var.set(tag)
    fut = asyncio.get_running_loop().create_future()
    asyncio.get_running_loop().call_soon_threadsafe(fut.set_result, 'done')
    await fut
    return f'{tag}={req_var.get()}'
",
            None,
            None,
        )
        .expect("define app");

        let c1 = py
            .eval(c"handler('A')", None, None)
            .expect("coro1")
            .unbind();
        let c2 = py
            .eval(c"handler('B')", None, None)
            .expect("coro2")
            .unbind();
        let r1 = harness.submit(py, c1);
        let r2 = harness.submit(py, c2);
        (harness, r1, r2)
    });

    let v1 = StreamingTestHarness::poll_result(&r1, POLL_TIMEOUT);
    let v2 = StreamingTestHarness::poll_result(&r2, POLL_TIMEOUT);
    harness.shutdown();

    match (&v1, &v2) {
        (Ok(a), Ok(b)) => {
            assert!(a.contains("A=A"), "request 1 got: {a}");
            assert!(b.contains("B=B"), "request 2 got: {b}");
        }
        _ => panic!("isolation test failed: r1={v1:?}, r2={v2:?}"),
    }
}

/// Async generator + sleep(0) pattern from the bench app `/stream/{chunks}`
/// endpoint. Multiple concurrent requests must complete without errors.
#[test]
fn stream_endpoint_async_generator_pattern() {
    crate::integration_tests::ensure_python_env();
    Python::initialize();

    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errors_clone = Arc::clone(&errors);

    let (mut harness, r1, r2, r3) = Python::attach(|py| {
        let harness = StreamingTestHarness::new(py);

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
        .expect("install error capture");
        let handler = py
            .eval(c"_stream_capture", None, None)
            .expect("get handler");
        harness
            .event_loop
            .call_method1(py, c"set_exception_handler", (handler,))
            .expect("set exception handler");

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
        .expect("define app");

        let c1 = py
            .eval(c"stream_handler()", None, None)
            .expect("coro1")
            .unbind();
        let c2 = py
            .eval(c"stream_handler()", None, None)
            .expect("coro2")
            .unbind();
        let c3 = py
            .eval(c"stream_handler()", None, None)
            .expect("coro3")
            .unbind();
        let r1 = harness.submit(py, c1);
        let r2 = harness.submit(py, c2);
        let r3 = harness.submit(py, c3);
        (harness, r1, r2, r3)
    });

    let v1 = StreamingTestHarness::poll_result(&r1, POLL_TIMEOUT);
    let v2 = StreamingTestHarness::poll_result(&r2, POLL_TIMEOUT);
    let v3 = StreamingTestHarness::poll_result(&r3, POLL_TIMEOUT);

    Python::attach(|py| {
        let captured: Vec<String> = py
            .eval(c"builtins._stream_errors", None, None)
            .expect("get errors")
            .extract()
            .expect("extract errors");
        let mut errs = errors_clone.lock().unwrap_or_else(|e| e.into_inner());
        errs.extend(captured);
    });

    harness.shutdown();

    let errs = errors.lock().unwrap_or_else(|e| e.into_inner());
    let enter_errors: Vec<_> = errs
        .iter()
        .filter(|e| e.contains("Cannot enter into task"))
        .collect();
    assert!(
        enter_errors.is_empty(),
        "stream_10 pattern produced {n} 'Cannot enter into task' errors:\n{errors:#?}",
        n = enter_errors.len(),
        errors = enter_errors,
    );

    assert!(v1.is_ok(), "request 1 failed: {v1:?}");
    assert!(v2.is_ok(), "request 2 failed: {v2:?}");
    assert!(v3.is_ok(), "request 3 failed: {v3:?}");

    let s1 = v1.expect("unwrap r1");
    assert!(
        s1.contains("chunk-0") && s1.contains("chunk-9"),
        "unexpected r1: {s1}"
    );
}

/// Same pattern but on uvloop — the production config.
/// Runs in a subprocess for clean uvloop state.
#[test]
fn stream_endpoint_uvloop_subprocess() {
    let exe = std::env::current_exe().expect("current exe");
    let output = std::process::Command::new(exe)
        .args([
            "integration_tests::streaming::stream_endpoint_uvloop_impl",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("APX_SUBPROCESS_TEST", "1")
        .output()
        .expect("spawn subprocess");
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

    let mut harness = Python::attach(|py| {
        let uvloop = py.import(c"uvloop").expect("import uvloop");
        let event_loop = uvloop
            .call_method0(c"new_event_loop")
            .expect("new_event_loop");
        let asyncio = py.import(c"asyncio").expect("import asyncio");
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .expect("set_event_loop");
        let events = py.import(c"asyncio.events").expect("import asyncio.events");
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .expect("_set_running_loop");

        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .expect("call_soon_threadsafe")
            .unbind();
        let create_task = event_loop
            .getattr(c"create_task")
            .expect("create_task")
            .unbind();

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
        .expect("install error capture");
        let handler = py.eval(c"_uv_capture", None, None).expect("get handler");
        event_loop
            .call_method1(c"set_exception_handler", (handler,))
            .expect("set exception handler");

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
        .expect("define app");

        let el_for_thread = event_loop.clone().unbind();
        let asyncio_thread = std::thread::Builder::new()
            .name("test-uvloop".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    let _ = el.call_method0(c"run_forever");
                });
            })
            .expect("spawn uvloop thread");

        StreamingTestHarness {
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            create_task,
            asyncio_thread: Some(asyncio_thread),
        }
    });

    // Submit 50 requests in batches of 10.
    for _ in 0..5 {
        Python::attach(|py| {
            for _ in 0..10 {
                let coro = py
                    .eval(c"stream_handler()", None, None)
                    .expect("create coro")
                    .unbind();
                let _ = harness.submit(py, coro);
            }
        });
        std::thread::sleep(Duration::from_millis(50));
    }

    // Wait for all tasks to complete.
    std::thread::sleep(Duration::from_millis(500));

    Python::attach(|py| {
        let captured: Vec<String> = py
            .eval(c"builtins._uv_errors", None, None)
            .expect("get errors")
            .extract()
            .expect("extract errors");
        let mut errs = errors_clone.lock().unwrap_or_else(|e| e.into_inner());
        errs.extend(captured);
    });

    harness.shutdown();

    let errs = errors.lock().unwrap_or_else(|e| e.into_inner());
    let enter_errors: Vec<_> = errs
        .iter()
        .filter(|e| e.contains("Cannot enter into task"))
        .collect();
    assert!(
        enter_errors.is_empty(),
        "stream_10 uvloop pattern produced {n} 'Cannot enter into task' errors:\n{errors:#?}",
        n = enter_errors.len(),
        errors = enter_errors,
    );
}
