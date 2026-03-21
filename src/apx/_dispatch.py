"""Zero-GIL dispatch loop with inline coroutine completion.

Installs an fd-based wakeup on the asyncio event loop (Unix) or
exposes a drain callback for ``call_soon_threadsafe`` (Windows).

The key optimization: for handlers that complete without yielding
(simple GET endpoints, sync dependency chains), the entire request
is driven inline via a single ``coro.send(None)`` — bypassing
``create_task`` and its ~7us of asyncio scheduling overhead.

Handlers that suspend (database queries, streaming) fall back to a
normal asyncio Task through ``_handle_suspension``, maintaining full
correctness for all ASGI handler patterns.

Called once from Rust during reactor init via
``py.import(c"apx._dispatch")?.call_method1(c"install_dispatch", ...)``.
"""

from __future__ import annotations

import asyncio
import asyncio.tasks
import logging
import os
import traceback
from collections.abc import Coroutine
from typing import Any, Callable

_log = logging.getLogger("apx._dispatch")

from apx._core import RequestQueue, SlotSend
from apx._task import _SmartSchedulerTask
from apx._wire import (
    OriginalCallSoon,
    enter_inline,
    flush_deferred_callbacks,
    install_call_soon_wire,
    leave_inline,
)

# Direct references to CPython internals — these are the same primitives
# that ``Task.__step`` uses to register / unregister the "current task"
# on the event loop.  Using them directly lets us bracket inline
# ``coro.send(None)`` so that ``asyncio.current_task()`` returns a real
# Task during handler execution (required by sniffio, anyio, Starlette).
_enter_task = asyncio.tasks._enter_task
_leave_task = asyncio.tasks._leave_task


def install_dispatch(
    loop: asyncio.AbstractEventLoop,
    queue: RequestQueue,
    app: Callable[..., Coroutine[Any, Any, None]],
    wakeup_fd: int | None = None,
) -> None:
    """Install the zero-GIL dispatch reader with inline completion.

    On Unix: registers ``wakeup_fd`` with the loop's selector via ``add_reader``.
    On Windows: ``wakeup_fd`` is ``None`` — Rust uses ``call_soon_threadsafe``
    which appends ``_drain_queue`` directly to ``_ready`` (no fd needed).

    The ``call_soon`` wire is installed once here and remains active for
    the lifetime of the event loop.  It captures ``call_soon`` callbacks
    during inline driving mode so they don't collide with the inline
    ``_enter_task`` / ``_leave_task`` bracket.
    """
    # Install the call_soon wire once — captures callbacks during inline
    # mode, passes through normally at all other times.
    original_call_soon: OriginalCallSoon = install_call_soon_wire(loop)

    def _handle_suspension(
        coro: Coroutine[Any, Any, None],
        yielded: Any,
        send: SlotSend,
        sched_task: asyncio.Task[None],
    ) -> None:
        """Drive a suspended coroutine to completion via callbacks.

        Python 3.11+ prevents ``await``-ing a coroutine that has
        already been started (``cr_await`` is set →
        ``RuntimeError: coroutine is being awaited already``).
        So we cannot wrap the half-driven coroutine in a new async
        wrapper.

        Instead we replicate ``Task.__step``'s yield classification
        with plain callbacks:

        - **Future** (``_asyncio_future_blocking`` is set):
          ``add_done_callback`` on the Future, resume when it resolves.
        - **None** (bare ``yield``):
          ``loop.call_soon(_resume)`` to reschedule.
        - **StopIteration**: coroutine completed — done.
        - **Exception**: forward via ``send.send_error``.

        Each ``coro.send()`` / ``coro.throw()`` is bracketed with
        ``_enter_task`` / ``_leave_task`` so that
        ``asyncio.current_task()`` returns the scheduler task —
        required by sniffio, anyio, and Starlette middleware.
        """

        def _classify_and_wait(value: Any) -> None:
            """Route a yielded value to the correct wait mechanism."""
            blocking = getattr(value, "_asyncio_future_blocking", None)
            if blocking is not None:
                # asyncio.Future — clear the protocol flag and wait.
                value._asyncio_future_blocking = False
                value.add_done_callback(_on_future_done)
            elif value is None:
                loop.call_soon(_resume)
            else:
                loop.call_soon(
                    _resume,
                    RuntimeError(f"coroutine yielded unexpected value: {value!r}"),
                )

        def _resume(exc: BaseException | None = None) -> None:
            """Send/throw into the coroutine, then classify the result."""
            _enter_task(loop, sched_task)
            try:
                if exc is None:
                    result = coro.send(None)
                else:
                    result = coro.throw(type(exc), exc, exc.__traceback__)
            except StopIteration:
                _leave_task(loop, sched_task)
                return
            except BaseException:
                _leave_task(loop, sched_task)
                tb = traceback.format_exc()
                _log.error("suspension exception:\n%s", tb)
                send.send_error(tb)
                return
            _leave_task(loop, sched_task)
            _classify_and_wait(result)

        def _on_future_done(fut: asyncio.Future[Any]) -> None:
            """Resume the coroutine after a Future resolves."""
            try:
                fut_exc = fut.exception()
            except asyncio.CancelledError as cancel_exc:
                _resume(cancel_exc)
                return
            if fut_exc is not None:
                _resume(fut_exc)
            else:
                _resume()

        _classify_and_wait(yielded)

    def _drain_queue() -> None:
        """Drain the inbound request queue with inline completion.

        For each queued request:

        1. Create the ASGI coroutine (``app(scope, receive, send)``).
        2. Attempt inline completion via ``coro.send(None)``.
           - ``StopIteration`` → handler completed in ~2us, no Task.
           - Exception → report error, continue to next request.
           - Yielded value → handler suspended, fall back to step 3.
        3. Fall back: wrap the suspended coroutine in a normal asyncio
           Task via ``_handle_suspension``.

        The ``_enter_task`` / ``_leave_task`` bracket around the inline
        ``send`` ensures ``asyncio.current_task()`` returns a real Task
        during handler execution — required by sniffio, anyio, and
        Starlette middleware that inspect the current task.

        The ``call_soon`` wire (enter_inline / leave_inline) captures
        any ``loop.call_soon`` callbacks emitted during inline driving
        (e.g. from ``Task.__init__``) and replays them after
        ``_leave_task`` — preventing ``_enter_task`` collisions on the
        asyncio thread.
        """
        while True:
            result = queue.try_recv()
            if result is None:
                break
            scope, receive, send = result
            coro: Coroutine[Any, Any, None] = app(scope, receive, send)

            # -- inline attempt -----------------------------------------------
            # Saves ~5us per request by avoiding create_task + Task.__step
            # + _run_once scheduling overhead.  Under 50 connections that's
            # 250us saved per _run_once batch — directly reducing the
            # queueing delay that causes the 6-8ms p50 latency gap vs uvicorn.
            sched_task = _SmartSchedulerTask(loop=loop)
            enter_inline()
            _enter_task(loop, sched_task)
            try:
                yielded = coro.send(None)
            except StopIteration:
                # Handler completed inline — entire request finished in one
                # coro.send(None) call.  No Task, no __step, no _run_once
                # scheduling cycles.  This is the fast path (~2us).
                _leave_task(loop, sched_task)
                leave_inline(original_call_soon)
                continue
            except BaseException:
                _leave_task(loop, sched_task)
                leave_inline(original_call_soon)
                tb = traceback.format_exc()
                _log.error("inline exception:\n%s", tb)
                send.send_error(tb)
                continue

            # -- handler suspended --------------------------------------------
            # The coroutine hit an await that isn't immediately resolvable
            # (database query, sleep, streaming, etc.).  Clean up the inline
            # bracket and fall back to normal asyncio Task machinery.
            _leave_task(loop, sched_task)
            flush_deferred_callbacks()
            leave_inline(original_call_soon)

            _log.debug(
                "handler suspended, yielded=%r, falling back to create_task",
                type(yielded).__name__,
            )
            _handle_suspension(coro, yielded, send, sched_task)

    if wakeup_fd is not None:

        def _on_readable() -> None:
            try:
                os.read(wakeup_fd, 4096)
            except BlockingIOError:
                pass
            _drain_queue()

        # add_reader is not thread-safe; schedule it onto the asyncio thread.
        loop.call_soon_threadsafe(loop.add_reader, wakeup_fd, _on_readable)
    else:
        install_dispatch._drain_queue = _drain_queue  # type: ignore[attr-defined]
