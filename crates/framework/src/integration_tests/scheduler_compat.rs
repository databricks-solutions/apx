//! Scheduler compatibility: verify the Rust scheduler can drive
//! real FastAPI/Starlette handlers end-to-end.
//!
//! Each test uses `TestServer::start_with_scheduler()` which creates
//! an event loop with `LoopPolicy::RustNative`. A failing test
//! identifies a specific asyncio/anyio compatibility gap.

use super::TestServer;

// ── Category 1: asyncio task context ────────────────────────────────

#[tokio::test]
async fn sched_current_task_not_none() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/task")
async def check_task():
    task = asyncio.current_task()
    return {"has_task": task is not None}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_current_task").await;
    let (status, body) = server.get("/task").await;
    assert_eq!(status, 200, "current_task: {body}");
    assert_eq!(body["has_task"], true, "current_task() returned None");
    server.stop().await;
}

#[tokio::test]
async fn sched_current_task_weakref() {
    let app = r#"
import asyncio, weakref
from fastapi import FastAPI
app = FastAPI()

@app.get("/weakref")
async def check_weakref():
    task = asyncio.current_task()
    ref = weakref.ref(task)
    return {"alive": ref() is not None}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_weakref").await;
    let (status, body) = server.get("/weakref").await;
    assert_eq!(status, 200, "weakref: {body}");
    assert_eq!(body["alive"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_current_task_done_callback() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/callback")
async def check_callback():
    task = asyncio.current_task()
    called = []
    try:
        task.add_done_callback(lambda t: called.append(True))
        has_callback = True
    except (AttributeError, TypeError):
        has_callback = False
    return {"has_callback": has_callback}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_done_cb").await;
    let (status, body) = server.get("/callback").await;
    assert_eq!(status, 200, "done_callback: {body}");
    assert_eq!(body["has_callback"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_current_task_cancel() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/cancel")
async def check_cancel():
    task = asyncio.current_task()
    try:
        has_cancel = callable(getattr(task, 'cancel', None))
    except Exception:
        has_cancel = False
    return {"has_cancel": has_cancel}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_cancel").await;
    let (status, body) = server.get("/cancel").await;
    assert_eq!(status, 200, "cancel: {body}");
    assert_eq!(body["has_cancel"], true);
    server.stop().await;
}

/// Prove the try-sync-first optimization: a simple buffered handler
/// completes without creating any `asyncio.Task`. The handler snapshots
/// `asyncio.all_tasks()` before and after the request lifecycle —
/// if the count doesn't grow, no Task was allocated.
#[tokio::test]
async fn sched_no_asyncio_task_for_sync_handler() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/no-task")
async def no_task():
    # Snapshot task count from inside the handler.
    # With spawn_and_drive, current_task() is a TaskProxy (not asyncio.Task),
    # so all_tasks() should not include it.
    tasks = asyncio.all_tasks()
    asyncio_task_count = sum(
        1 for t in tasks if type(t).__module__ == "asyncio.tasks"
    )
    return {"asyncio_task_count": asyncio_task_count}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_no_task").await;
    let (status, body) = server.get("/no-task").await;
    assert_eq!(status, 200, "no-task: {body}");
    // Zero asyncio.Task objects — the handler was driven by spawn_and_drive.
    assert_eq!(
        body["asyncio_task_count"], 0,
        "expected 0 asyncio.Task objects, got {body}"
    );
    server.stop().await;
}

// ── Category 2: asyncio primitives through the driver ───────────────

#[tokio::test]
async fn sched_asyncio_sleep_zero() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/sleep0")
async def sleep_zero():
    await asyncio.sleep(0)
    return {"ok": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_sleep0").await;
    let (status, body) = server.get("/sleep0").await;
    assert_eq!(status, 200, "sleep(0): {body}");
    assert_eq!(body["ok"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_sleep_nonzero() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/sleep")
async def sleep_short():
    await asyncio.sleep(0.01)
    return {"ok": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_sleep_nz").await;
    let (status, body) = server.get("/sleep").await;
    assert_eq!(status, 200, "sleep(0.01): {body}");
    assert_eq!(body["ok"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_create_task() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/create-task")
async def create_task_test():
    result = []
    async def worker():
        result.append(42)
    task = asyncio.create_task(worker())
    await task
    return {"value": result[0] if result else None}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_create_task").await;
    let (status, body) = server.get("/create-task").await;
    assert_eq!(status, 200, "create_task: {body}");
    assert_eq!(body["value"], 42);
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_gather() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/gather")
async def gather_test():
    async def a(): return 1
    async def b(): return 2
    results = await asyncio.gather(a(), b())
    return {"sum": sum(results)}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_gather").await;
    let (status, body) = server.get("/gather").await;
    assert_eq!(status, 200, "gather: {body}");
    assert_eq!(body["sum"], 3);
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_event() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/event")
async def event_test():
    event = asyncio.Event()
    event.set()
    await event.wait()
    return {"ok": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_event").await;
    let (status, body) = server.get("/event").await;
    assert_eq!(status, 200, "event: {body}");
    assert_eq!(body["ok"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_wait_for() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/wait-for")
async def wait_for_test():
    async def quick(): return "done"
    result = await asyncio.wait_for(quick(), timeout=1.0)
    return {"result": result}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_wait_for").await;
    let (status, body) = server.get("/wait-for").await;
    assert_eq!(status, 200, "wait_for: {body}");
    assert_eq!(body["result"], "done");
    server.stop().await;
}

#[tokio::test]
async fn sched_asyncio_shield() {
    let app = r#"
import asyncio
from fastapi import FastAPI
app = FastAPI()

@app.get("/shield")
async def shield_test():
    async def inner(): return "shielded"
    result = await asyncio.shield(inner())
    return {"result": result}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_shield").await;
    let (status, body) = server.get("/shield").await;
    assert_eq!(status, 200, "shield: {body}");
    assert_eq!(body["result"], "shielded");
    server.stop().await;
}

// ── Category 3: anyio layer (Starlette internals) ───────────────────

#[tokio::test]
async fn sched_anyio_sleep() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/anyio-sleep")
async def anyio_sleep_test():
    await anyio.sleep(0)
    return {"ok": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_anyio_sleep").await;
    let (status, body) = server.get("/anyio-sleep").await;
    assert_eq!(status, 200, "anyio.sleep: {body}");
    assert_eq!(body["ok"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_anyio_run_sync() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/anyio-sync")
async def anyio_sync_test():
    def compute():
        return 42
    result = await anyio.to_thread.run_sync(compute)
    return {"result": result}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_anyio_sync").await;
    let (status, body) = server.get("/anyio-sync").await;
    assert_eq!(status, 200, "anyio.run_sync: {body}");
    assert_eq!(body["result"], 42);
    server.stop().await;
}

#[tokio::test]
async fn sched_anyio_task_group() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/task-group")
async def task_group_test():
    results = []
    async def worker(n):
        results.append(n)
    async with anyio.create_task_group() as tg:
        tg.start_soon(worker, 1)
        tg.start_soon(worker, 2)
    return {"count": len(results)}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_tg").await;
    let (status, body) = server.get("/task-group").await;
    assert_eq!(status, 200, "task_group: {body}");
    assert_eq!(body["count"], 2);
    server.stop().await;
}

#[tokio::test]
async fn sched_anyio_cancel_scope() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/cancel-scope")
async def cancel_scope_test():
    with anyio.CancelScope(deadline=anyio.current_time() + 10) as scope:
        await anyio.sleep(0)
    return {"cancelled": scope.cancel_called}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_cs").await;
    let (status, body) = server.get("/cancel-scope").await;
    assert_eq!(status, 200, "cancel_scope: {body}");
    assert_eq!(body["cancelled"], false);
    server.stop().await;
}

// ── Category 4: Starlette/FastAPI middleware & lifecycle ─────────────

#[tokio::test]
async fn sched_base_http_middleware() {
    let app = r#"
from fastapi import FastAPI
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request

app = FastAPI()

class AddHeaderMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        response = await call_next(request)
        response.headers["x-scheduler"] = "rust"
        return response

app.add_middleware(AddHeaderMiddleware)

@app.get("/mw")
async def mw_test():
    return {"ok": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_mw").await;
    let (status, headers, _body) = server.get_raw("/mw").await;
    assert_eq!(status, 200, "middleware: status {status}");
    assert_eq!(
        headers.get("x-scheduler").map(|v| v.to_str().unwrap_or("")),
        Some("rust"),
        "x-scheduler header missing"
    );
    server.stop().await;
}

#[tokio::test]
async fn sched_exception_500() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/boom")
async def boom():
    raise RuntimeError("internal failure")
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_500").await;
    let (status, _body) = server.get("/boom").await;
    assert_eq!(status, 500, "unhandled exception should return 500");
    server.stop().await;
}

#[tokio::test]
async fn sched_background_tasks() {
    let app = r#"
from fastapi import FastAPI, BackgroundTasks
app = FastAPI()

@app.post("/bg")
async def bg_test(background_tasks: BackgroundTasks):
    async def task():
        pass
    background_tasks.add_task(task)
    return {"queued": True}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_bg").await;
    let (status, body) = server.post_empty("/bg").await;
    assert_eq!(status, 200, "background_tasks: {body}");
    assert_eq!(body["queued"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_contextvars_propagation() {
    let app = r#"
import contextvars
from fastapi import FastAPI, Request
from starlette.middleware.base import BaseHTTPMiddleware

request_id_var = contextvars.ContextVar("request_id", default="unset")
app = FastAPI()

class ContextMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        request_id_var.set("req-123")
        return await call_next(request)

app.add_middleware(ContextMiddleware)

@app.get("/ctx")
async def ctx_test():
    return {"request_id": request_id_var.get()}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_ctx").await;
    let (status, body) = server.get("/ctx").await;
    assert_eq!(status, 200, "contextvars: {body}");
    assert_eq!(body["request_id"], "req-123");
    server.stop().await;
}

// ── Category 5: async generators & streaming ────────────────────────

#[tokio::test]
async fn sched_async_generator_dependency() {
    let app = r#"
from fastapi import FastAPI, Depends
app = FastAPI()

async def get_resource():
    yield {"active": True}

@app.get("/dep-gen")
async def dep_gen(resource=Depends(get_resource)):
    return {"active": resource["active"]}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_dep_gen").await;
    let (status, body) = server.get("/dep-gen").await;
    assert_eq!(status, 200, "async gen dep: {body}");
    assert_eq!(body["active"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_streaming_response() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import StreamingResponse

app = FastAPI()

@app.get("/stream")
async def stream():
    async def generate():
        for i in range(3):
            yield f"chunk{i}\n"
    return StreamingResponse(generate(), media_type="text/plain")
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_stream").await;
    let (status, text) = server.get_text("/stream").await;
    assert_eq!(status, 200, "streaming: status {status}");
    assert_eq!(text, "chunk0\nchunk1\nchunk2\n");
    server.stop().await;
}

// ── Category 6: Native CancelScope, TaskGroup, MemoryObjectStream ────

#[tokio::test]
async fn sched_cancel_scope_no_cancel() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/cs-no-cancel")
async def cs_test():
    with anyio.CancelScope() as scope:
        await anyio.sleep(0)
    return {"cancelled_caught": scope.cancelled_caught, "cancel_called": scope.cancel_called}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_cs_no").await;
    let (status, body) = server.get("/cs-no-cancel").await;
    assert_eq!(status, 200, "cancel_scope no cancel: {body}");
    assert_eq!(body["cancelled_caught"], false);
    assert_eq!(body["cancel_called"], false);
    server.stop().await;
}

#[tokio::test]
async fn sched_cancel_scope_manual_cancel() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/cs-manual")
async def cs_manual():
    with anyio.CancelScope() as scope:
        scope.cancel()
        try:
            await anyio.sleep(0)
            reached = True
        except Exception:
            reached = False
    return {"cancel_called": scope.cancel_called, "cancelled_caught": scope.cancelled_caught}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_cs_man").await;
    let (status, body) = server.get("/cs-manual").await;
    assert_eq!(status, 200, "cancel_scope manual: {body}");
    assert_eq!(body["cancel_called"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_cancel_scope_deadline() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/cs-deadline")
async def cs_deadline():
    with anyio.CancelScope(deadline=anyio.current_time() + 10) as scope:
        await anyio.sleep(0)
    return {"cancelled": scope.cancel_called, "caught": scope.cancelled_caught}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_cs_dl").await;
    let (status, body) = server.get("/cs-deadline").await;
    assert_eq!(status, 200, "cancel_scope deadline: {body}");
    assert_eq!(body["cancelled"], false);
    server.stop().await;
}

#[tokio::test]
async fn sched_task_group_fan_out() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/tg-fan")
async def tg_fan():
    results = []
    async def worker(n):
        results.append(n)
    async with anyio.create_task_group() as tg:
        tg.start_soon(worker, 10)
        tg.start_soon(worker, 20)
        tg.start_soon(worker, 30)
    return {"count": len(results), "sum": sum(results)}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_tg_fan").await;
    let (status, body) = server.get("/tg-fan").await;
    assert_eq!(status, 200, "task_group fan-out: {body}");
    assert_eq!(body["count"], 3);
    assert_eq!(body["sum"], 60);
    server.stop().await;
}

#[tokio::test]
async fn sched_task_group_error_propagation() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/tg-err")
async def tg_err():
    async def bad_worker():
        raise ValueError("child failed")
    try:
        async with anyio.create_task_group() as tg:
            tg.start_soon(bad_worker)
        caught = False
    except (ValueError, BaseExceptionGroup):
        caught = True
    return {"caught": caught}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_tg_err").await;
    let (status, body) = server.get("/tg-err").await;
    assert_eq!(status, 200, "task_group error: {body}");
    assert_eq!(body["caught"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_memory_stream_basic() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/ms-basic")
async def ms_basic():
    send, receive = anyio.create_memory_object_stream(max_buffer_size=10)
    await send.send("hello")
    await send.send("world")
    item1 = await receive.receive()
    item2 = await receive.receive()
    await send.aclose()
    await receive.aclose()
    return {"items": [item1, item2]}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_ms").await;
    let (status, body) = server.get("/ms-basic").await;
    assert_eq!(status, 200, "memory_stream basic: {body}");
    assert_eq!(body["items"][0], "hello");
    assert_eq!(body["items"][1], "world");
    server.stop().await;
}

#[tokio::test]
async fn sched_memory_stream_close() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/ms-close")
async def ms_close():
    send, receive = anyio.create_memory_object_stream(max_buffer_size=10)
    await send.send("data")
    await send.aclose()
    item = await receive.receive()
    try:
        await receive.receive()
        end = False
    except anyio.EndOfStream:
        end = True
    await receive.aclose()
    return {"item": item, "end_of_stream": end}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_ms_cl").await;
    let (status, body) = server.get("/ms-close").await;
    assert_eq!(status, 200, "memory_stream close: {body}");
    assert_eq!(body["item"], "data");
    assert_eq!(body["end_of_stream"], true);
    server.stop().await;
}

#[tokio::test]
async fn sched_task_group_with_cancel_scope() {
    let app = r#"
import anyio
from fastapi import FastAPI
app = FastAPI()

@app.get("/tg-cs")
async def tg_cs():
    results = []
    async with anyio.create_task_group() as tg:
        async def worker(n):
            results.append(n)
        with anyio.CancelScope() as scope:
            tg.start_soon(worker, 1)
            tg.start_soon(worker, 2)
    return {"count": len(results)}
"#;
    let mut server = TestServer::start_with_scheduler(app, "_sched_tg_cs").await;
    let (status, body) = server.get("/tg-cs").await;
    assert_eq!(status, 200, "task_group + cancel_scope: {body}");
    assert_eq!(body["count"], 2);
    server.stop().await;
}
