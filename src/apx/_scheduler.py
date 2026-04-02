"""Inline coroutine driver for ASGI request handling.

Drives ASGI coroutines to completion within a single ``_run_once``
callback, eliminating ``create_task`` scheduling overhead.  Falls back
to callback-based continuation for coroutines that suspend on real I/O.

Safety: all driving happens on the asyncio thread during callback
processing (``current_task() is None``).  Per-step ``_enter_task`` /
``_leave_task`` brackets maintain invariant I1.  See
``.plans/framework/io/pythonic-inlining.md`` for the full analysis.
"""

from __future__ import annotations

import asyncio
import contextvars
import time
from collections import deque
from collections.abc import Callable, Coroutine
from typing import Any

# ── Constants ────────────────────────────────────────────────────────

STEP_BUDGET: int = 256
"""Maximum coroutine steps before falling back to continuation."""

TIME_BUDGET_S: float = 0.005
"""Maximum wall-clock seconds for inline driving (5ms)."""

FLUSH_BUDGET: int = 64
"""Maximum captured callbacks to process between drive steps."""

# ── asyncio internals (stable since 3.7) ────────────────────────────

_enter_task = asyncio.tasks._enter_task  # type: ignore[attr-defined]
_leave_task = asyncio.tasks._leave_task  # type: ignore[attr-defined]


# ── Scheduler task ───────────────────────────────────────────────────


async def _park_forever() -> None:
    """Sentinel coroutine that parks on an unresolved Future."""
    await asyncio.get_event_loop().create_future()


class SchedulerTask(asyncio.Task):
    """Placeholder task for ``_enter_task`` / ``_leave_task`` bracketing.

    Parks on an unresolved Future (``done() == False``).  Provides
    cancel forwarding so ``asyncio.timeout`` and ``anyio.fail_after``
    can signal the inline driver or continuation.
    """

    __slots__ = (
        "_cancel_flag",
        "_cancel_msg",
        "_cancel_count",
        "_waiter",
        "_drive_context",
    )

    def __init__(self, *, loop: asyncio.AbstractEventLoop) -> None:
        # Capture context BEFORE super().__init__ which may alter it.
        # We store it explicitly because CPython's C-implemented Task
        # does not expose ``_context`` as a Python-accessible attribute.
        self._drive_context: contextvars.Context = contextvars.copy_context()
        super().__init__(_park_forever(), loop=loop)
        self._log_destroy_pending: bool = False
        self._cancel_flag: bool = False
        self._cancel_msg: str | None = None
        self._cancel_count: int = 0
        self._waiter: asyncio.Future[Any] | None = None

    def cancel(self, msg: str | None = None) -> bool:
        self._cancel_flag = True
        self._cancel_msg = msg
        self._cancel_count += 1
        if self._waiter is not None and not self._waiter.done():
            self._waiter.cancel(msg=msg)
        return True

    def cancelling(self) -> int:
        return self._cancel_count

    def uncancel(self) -> int:
        self._cancel_count = max(0, self._cancel_count - 1)
        return self._cancel_count


# ── call_soon capture ────────────────────────────────────────────────


class CallSoonCapture:
    """Intercepts ``loop.call_soon`` during inline driving.

    While active, callbacks are captured into an internal queue instead
    of being appended to the event loop's ``_ready`` deque.  This
    prevents the sentinel ``__step`` from ``SchedulerTask.__init__``
    (invariant I7) from polluting ``_run_once``.

    Captured callbacks are processed between drive steps via
    ``flush()`` or spilled back to the real ``call_soon`` on ``leave()``.
    """

    __slots__ = ("_original", "_queue", "_active")

    # Queue entry: (callback, args, context).  Context is preserved so
    # that Task.__step and Future done-callbacks run in their correct
    # contextvars snapshot (invariant I2).
    _Entry = tuple[Callable[..., Any], tuple[Any, ...], contextvars.Context | None]

    def __init__(self, loop: asyncio.AbstractEventLoop) -> None:
        self._original: Callable[..., Any] = loop.call_soon
        self._queue: deque[CallSoonCapture._Entry] = deque()
        self._active: bool = False
        loop.call_soon = self._intercept  # type: ignore[assignment, ty:invalid-assignment]

    def _intercept(
        self,
        callback: Callable[..., Any],
        *args: Any,
        context: contextvars.Context | None = None,
    ) -> None:
        if self._active:
            self._queue.append((callback, args, context))
        else:
            self._original(callback, *args, context=context)

    def enter(self) -> None:
        """Start capturing ``call_soon`` callbacks."""
        self._active = True
        self._queue.clear()

    def leave(self) -> None:
        """Stop capturing and spill remaining callbacks to the real loop."""
        self._active = False
        original = self._original
        while self._queue:
            cb, args, ctx = self._queue.popleft()
            original(cb, *args, context=ctx)

    def flush(self, budget: int = FLUSH_BUDGET) -> None:
        """Process captured callbacks inline (between drive steps).

        Callbacks run in their original context so that contextvars
        (e.g. OTEL trace propagation) are preserved correctly.
        """
        queue = self._queue
        while queue and budget > 0:
            cb, args, ctx = queue.popleft()
            if ctx is not None:
                ctx.run(cb, *args)
            else:
                cb(*args)
            budget -= 1


# ── Inline driver ────────────────────────────────────────────────────


class _DriveResult:
    """Base class for inline drive outcomes."""


class Completed(_DriveResult):
    """Coroutine ran to completion."""


class Suspended(_DriveResult):
    """Coroutine yielded an asyncio Future (real I/O)."""

    __slots__ = ("yielded",)

    def __init__(self, yielded: object) -> None:
        self.yielded = yielded


class Failed(_DriveResult):
    """Coroutine raised an exception."""

    __slots__ = ("exc",)

    def __init__(self, exc: BaseException) -> None:
        self.exc = exc


_COMPLETED = Completed()


def drive_inline(
    coro: Coroutine[Any, Any, Any],
    task: SchedulerTask,
    loop: asyncio.AbstractEventLoop,
    capture: CallSoonCapture,
    *,
    send_value: Any = None,
    send_exception: BaseException | None = None,
) -> _DriveResult:
    """Drive a coroutine to completion or first real suspension.

    Must be called from a ``_run_once`` callback where
    ``current_task() is None``.  Uses per-step ``_enter_task`` /
    ``_leave_task`` brackets (invariant I1).

    On initial entry ``send_value`` is ``None`` (starts the coroutine).
    On continuation re-entry after a Future resolves, pass the Future's
    result via ``send_value`` or its exception via ``send_exception``
    so the coroutine receives the I/O result at its ``await`` point.

    Returns:
        Completed — coroutine finished; response already fired.
        Suspended — coroutine yielded an asyncio Future; needs
                    callback-based continuation.
        Failed    — coroutine raised; caller should log / error.
    """
    budget = STEP_BUDGET
    deadline = time.monotonic() + TIME_BUDGET_S
    context_run = task._drive_context.run

    while True:
        _enter_task(loop, task)
        try:
            if send_exception is not None:
                result = context_run(coro.throw, send_exception)
                send_exception = None
            else:
                result = context_run(coro.send, send_value)
                send_value = None
        except StopIteration:
            _leave_task(loop, task)
            return _COMPLETED
        except BaseException as exc:
            _leave_task(loop, task)
            return Failed(exc)
        _leave_task(loop, task)

        if result is not None and getattr(result, "_asyncio_future_blocking", False):
            result._asyncio_future_blocking = False
            return Suspended(result)

        capture.flush()

        if result is None:
            budget -= 1
            if budget <= 0 or time.monotonic() > deadline:
                return Suspended(None)
            continue

        return Suspended(result)
