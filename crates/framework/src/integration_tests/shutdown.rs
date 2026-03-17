//! Shutdown sequence integration tests.
//!
//! These tests verify that `Reactor::shutdown` properly calls
//! `shutdown_asyncgens` and `shutdown_default_executor` before closing
//! the event loop. Tests run Python scripts in subprocesses to get
//! full event loop lifecycle isolation.

use std::process::Command;
use std::time::{Duration, Instant};

/// Run a Python script via `uv run python -c` and return (stdout, stderr, success).
fn run_python(code: &str) -> (String, String, bool) {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = Command::new("uv")
        .args(["run", "python", "-c", code])
        .current_dir(workspace_root)
        .output()
        .expect("failed to run uv");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn shutdown_asyncgens_runs_finally_blocks() {
    let (stdout, stderr, success) = run_python(
        r#"
import asyncio

finalized = False

async def agen():
    global finalized
    try:
        yield 1
        yield 2
    finally:
        finalized = True

async def main():
    g = agen()
    await g.__anext__()  # get first value, abandon generator

asyncio.run(main())
# asyncio.run() calls shutdown_asyncgens, which triggers the finally block.
print(f"finalized={finalized}")
"#,
    );

    assert!(
        success,
        "script failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.trim().contains("finalized=True"),
        "async generator finally block should have run:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn shutdown_default_executor_completes_work() {
    let (stdout, stderr, success) = run_python(
        r#"
import asyncio
import time

result = []

def blocking_work():
    time.sleep(0.1)
    result.append("done")
    return "ok"

async def main():
    loop = asyncio.get_running_loop()
    await loop.run_in_executor(None, blocking_work)

asyncio.run(main())
print(f"result={result}")
"#,
    );

    assert!(
        success,
        "script failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.trim().contains("result=['done']"),
        "executor work should have completed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn shutdown_executor_timeout_does_not_hang() {
    let start = Instant::now();
    let (stdout, stderr, success) = run_python(
        r#"
import asyncio
import time

async def main():
    loop = asyncio.get_running_loop()
    # Submit long-running work but don't await it — shutdown_default_executor
    # should handle it (either wait or timeout).
    loop.run_in_executor(None, lambda: time.sleep(0.5))
    # Return immediately — shutdown_default_executor runs during cleanup.

asyncio.run(main())
print("shutdown_complete")
"#,
    );
    let elapsed = start.elapsed();

    assert!(
        success,
        "script failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.trim().contains("shutdown_complete"),
        "shutdown should have completed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "shutdown took {elapsed:?} — should complete in <10s"
    );
}

#[test]
fn cancel_pending_tasks_survives_gc_during_iteration() {
    let (stdout, stderr, success) = run_python(
        r#"
import asyncio
import gc

async def main():
    tasks = []
    for _ in range(50):
        tasks.append(asyncio.ensure_future(asyncio.sleep(60)))
    # Drop all local references — tasks only held by asyncio._all_tasks WeakSet.
    tasks.clear()
    # Force GC to collect weakly-referenced tasks during shutdown.
    gc.collect()
    # asyncio.run() calls cancel_pending_tasks internally — must not raise
    # "Set changed size during iteration".

asyncio.run(main())
print("ok")
"#,
    );

    assert!(
        success,
        "script failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.trim().contains("ok"),
        "cancel_pending_tasks should survive GC pressure:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Set changed size during iteration"),
        "WeakSet iteration error detected:\nstderr: {stderr}"
    );
}
