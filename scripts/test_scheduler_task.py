"""Test _SchedulerTask approach for TaskProxy replacement.

Key findings:
1. asyncio.Task's C accelerator makes fields read-only — must call super().__init__()
2. _enter_task is exclusive — can't enter while another task is "current"
3. Tests must run OUTSIDE an asyncio task (matching APX production: Rust thread, no current task)

Usage: uv run scripts/test_scheduler_task.py
"""

from __future__ import annotations

import asyncio
import asyncio.tasks
import sys
import threading
import time
import traceback
import weakref

# ---------------------------------------------------------------------------
# The proposed _SchedulerTask
# ---------------------------------------------------------------------------


async def _sentinel():
    """Suspend forever — the Rust scheduler drives the real coroutine."""
    await asyncio.get_running_loop().create_future()


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for the Rust scheduler.

    Calls super().__init__() with a suspended sentinel to properly init
    C struct fields. The real coroutine is driven externally.
    """

    def __init__(self, coro, *, loop=None):
        super().__init__(_sentinel(), loop=loop)
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self):
        return self._real_coro


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------

_enter_task = asyncio.tasks._enter_task
_leave_task = asyncio.tasks._leave_task
PASS = 0
FAIL = 0


def check(name: str, condition: bool, detail: str = ""):
    global PASS, FAIL
    if condition:
        PASS += 1
        print(f"  PASS: {name}")
    else:
        FAIL += 1
        print(f"  FAIL: {name} — {detail}")


# ---------------------------------------------------------------------------
# Tests — run on a bare thread (no asyncio task), like the APX tokio thread
# ---------------------------------------------------------------------------


def test_enter_leave_on_bare_thread():
    """_enter_task / _leave_task on a thread with no current asyncio task.
    This matches APX production: tokio thread has _set_running_loop but no current task."""
    print("\n[test_enter_leave_on_bare_thread]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)
    try:
        coro = asyncio.sleep(0)
        task = _SchedulerTask(coro, loop=loop)

        check("isinstance asyncio.Task", isinstance(task, asyncio.Task))
        check("no current task initially", asyncio.current_task() is None)

        _enter_task(loop, task)
        check("current_task is proxy", asyncio.current_task() is task)

        _leave_task(loop, task)
        check("current_task is None after leave", asyncio.current_task() is None)

        task.cancel()
        coro.close()
    except Exception as e:
        check("no exception", False, f"{e}\n{traceback.format_exc()}")
    finally:
        asyncio._set_running_loop(None)
        # Cancel pending tasks from super().__init__
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_weakref_on_bare_thread():
    """Starlette middleware creates weakrefs to asyncio.current_task()."""
    print("\n[test_weakref_on_bare_thread]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)
    try:
        coro = asyncio.sleep(0)
        task = _SchedulerTask(coro, loop=loop)

        ref = weakref.ref(task)
        check("weakref created", ref() is task)

        _enter_task(loop, task)
        current = asyncio.current_task()
        ref2 = weakref.ref(current)
        check("weakref from current_task", ref2() is task)
        _leave_task(loop, task)

        task.cancel()
        coro.close()
    except Exception as e:
        check("no exception", False, f"{e}\n{traceback.format_exc()}")
    finally:
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_attributes_on_bare_thread():
    """Task attributes accessed by middleware."""
    print("\n[test_attributes_on_bare_thread]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)
    try:
        coro = asyncio.sleep(0)
        task = _SchedulerTask(coro, loop=loop)

        check("get_name()", task.get_name() is not None)
        check("get_coro() is real coro", task.get_coro() is coro)
        check("done()", not task.done())
        check("cancelled()", not task.cancelled())
        check("_fut_waiter is None", task._fut_waiter is None)
        check("cancelling() == 0", task.cancelling() == 0)
        check("repr no crash", "Task" in repr(task) or "Scheduler" in repr(task))

        task.cancel()
        coro.close()
    except Exception as e:
        check("no exception", False, f"{e}\n{traceback.format_exc()}")
    finally:
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_100_enter_leave_cycles():
    """Simulate drain loop: 100 enter/leave cycles."""
    print("\n[test_100_enter_leave_cycles]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)
    try:
        for i in range(100):
            coro = asyncio.sleep(0)
            task = _SchedulerTask(coro, loop=loop)
            _enter_task(loop, task)
            assert asyncio.current_task() is task
            _leave_task(loop, task)
            task.cancel()
            coro.close()

        check("100 cycles clean", asyncio.current_task() is None)
    except Exception as e:
        check("no exception", False, f"{e}\n{traceback.format_exc()}")
    finally:
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_real_tasks_step_after_leave():
    """After _leave_task clears the proxy, real asyncio.Tasks can be stepped.
    Uses a background asyncio thread (like APX production)."""
    print("\n[test_real_tasks_step_after_leave]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)

    # Start asyncio thread (like APX's apx-asyncio thread)
    asyncio_thread = threading.Thread(
        target=loop.run_forever, name="test-asyncio", daemon=True
    )
    asyncio_thread.start()

    try:
        result_holder = {}

        async def inner_task():
            result_holder["value"] = "stepped_ok"

        coro = asyncio.sleep(0)
        proxy = _SchedulerTask(coro, loop=loop)

        # Simulate: Rust scheduler enters, creates real task, leaves
        _enter_task(loop, proxy)

        # Create real task (like anyio does)
        future = asyncio.run_coroutine_threadsafe(inner_task(), loop)

        _leave_task(loop, proxy)

        # Wait for the real task to complete on the asyncio thread
        future.result(timeout=5)

        check("real task stepped", result_holder.get("value") == "stepped_ok")

        proxy.cancel()
        coro.close()
    except Exception as e:
        check("no exception", False, f"{e}\n{traceback.format_exc()}")
    finally:
        loop.call_soon_threadsafe(loop.stop)
        asyncio_thread.join(timeout=5)
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_streaming_pattern_with_asyncio_thread():
    """Full production pattern: proxy enter/leave + concurrent tasks + asyncio thread."""
    print("\n[test_streaming_pattern_with_asyncio_thread]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)

    asyncio_thread = threading.Thread(
        target=loop.run_forever, name="test-asyncio-2", daemon=True
    )
    asyncio_thread.start()

    try:
        coro = asyncio.sleep(0)
        proxy = _SchedulerTask(coro, loop=loop)

        # Phase 1: Enter proxy (Rust scheduler driving)
        _enter_task(loop, proxy)

        # Phase 2: Create concurrent tasks (like Starlette StreamingResponse)
        results = []

        async def producer():
            for i in range(5):
                results.append(f"chunk-{i}")
                await asyncio.sleep(0)

        async def listener():
            await asyncio.sleep(0.05)

        async def orchestrator():
            p = asyncio.create_task(producer())
            l = asyncio.create_task(listener())
            await p
            l.cancel()
            try:
                await l
            except asyncio.CancelledError:
                pass

        # Phase 3: Leave proxy before suspension
        _leave_task(loop, proxy)

        # Phase 4: Run the orchestrator on the asyncio thread
        future = asyncio.run_coroutine_threadsafe(orchestrator(), loop)
        future.result(timeout=5)

        result = ",".join(results)
        check("all chunks", "chunk-0" in result and "chunk-4" in result, result)
        check(
            "correct result",
            result == "chunk-0,chunk-1,chunk-2,chunk-3,chunk-4",
            result,
        )

        proxy.cancel()
        coro.close()
    except Exception as e:
        check("streaming pattern", False, f"{e}\n{traceback.format_exc()}")
    finally:
        loop.call_soon_threadsafe(loop.stop)
        asyncio_thread.join(timeout=5)
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_sentinel_step_doesnt_conflict():
    """The sentinel from super().__init__ gets stepped by the asyncio thread.
    It must NOT interfere with our _enter_task/_leave_task calls."""
    print("\n[test_sentinel_step_doesnt_conflict]")

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)

    asyncio_thread = threading.Thread(
        target=loop.run_forever, name="test-asyncio-3", daemon=True
    )
    asyncio_thread.start()

    try:
        coro = asyncio.sleep(0)
        proxy = _SchedulerTask(coro, loop=loop)

        # Let the asyncio thread step the sentinel (call_soon from __init__)
        time.sleep(0.05)

        # Now enter/leave — should work even after sentinel was stepped
        _enter_task(loop, proxy)
        check("enter after sentinel step", asyncio.current_task() is proxy)
        _leave_task(loop, proxy)
        check("leave after sentinel step", asyncio.current_task() is None)

        # Enter/leave again — verify it's repeatable
        _enter_task(loop, proxy)
        _leave_task(loop, proxy)
        check("second cycle clean", asyncio.current_task() is None)

        proxy.cancel()
        coro.close()
    except Exception as e:
        check("sentinel doesn't conflict", False, f"{e}\n{traceback.format_exc()}")
    finally:
        loop.call_soon_threadsafe(loop.stop)
        asyncio_thread.join(timeout=5)
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


def test_anyio_task_group():
    """Test with actual anyio (the library Starlette uses)."""
    print("\n[test_anyio_task_group]")
    try:
        import anyio
    except ImportError:
        print("  SKIP: anyio not installed")
        return

    loop = asyncio.new_event_loop()
    asyncio._set_running_loop(loop)

    asyncio_thread = threading.Thread(
        target=loop.run_forever, name="test-asyncio-4", daemon=True
    )
    asyncio_thread.start()

    try:
        coro = asyncio.sleep(0)
        proxy = _SchedulerTask(coro, loop=loop)

        _enter_task(loop, proxy)
        _leave_task(loop, proxy)

        results = []

        async def with_anyio():
            async def producer():
                for i in range(3):
                    results.append(f"chunk-{i}")
                    await asyncio.sleep(0)

            async def consumer():
                await asyncio.sleep(0)

            async with anyio.create_task_group() as tg:
                tg.start_soon(producer)
                await consumer()
                tg.cancel_scope.cancel()

        future = asyncio.run_coroutine_threadsafe(with_anyio(), loop)
        future.result(timeout=5)

        check("anyio chunks produced", len(results) > 0, f"got {results}")

        proxy.cancel()
        coro.close()
    except Exception as e:
        check("anyio task group", False, f"{e}\n{traceback.format_exc()}")
    finally:
        loop.call_soon_threadsafe(loop.stop)
        asyncio_thread.join(timeout=5)
        asyncio._set_running_loop(None)
        pending = asyncio.all_tasks(loop)
        for t in pending:
            t.cancel()
        loop.close()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    print(f"Python {sys.version}")
    try:
        import _asyncio

        print(f"C accelerator: yes (_enter_task type: {type(_enter_task).__name__})")
    except ImportError:
        print(f"C accelerator: no (pure Python fallback)")

    test_enter_leave_on_bare_thread()
    test_weakref_on_bare_thread()
    test_attributes_on_bare_thread()
    test_100_enter_leave_cycles()
    test_real_tasks_step_after_leave()
    test_streaming_pattern_with_asyncio_thread()
    test_sentinel_step_doesnt_conflict()
    test_anyio_task_group()

    print(f"\n{'=' * 60}")
    print(f"Results: {PASS} passed, {FAIL} failed")
    if FAIL:
        sys.exit(1)
    print("All checks passed!")
