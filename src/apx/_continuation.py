"""Callback-based continuation for suspended coroutines.

When ``drive_inline`` returns ``Suspended``, the coroutine has yielded
an asyncio Future (real I/O like a database query).  ``Continuation``
attaches a done-callback to that Future and resumes driving when the
I/O completes — entirely on the asyncio thread, no ``create_task``.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable, Coroutine
from typing import Any

from apx._scheduler import (
    CallSoonCapture,
    Completed,
    Failed,
    SchedulerTask,
    Suspended,
    _enter_task,
    _leave_task,
    drive_inline,
)


class Continuation:
    """Drives a suspended coroutine via done-callbacks.

    Each step uses per-step ``_enter_task`` / ``_leave_task`` brackets,
    keeping invariant I1.  Runs entirely on the asyncio thread.
    """

    __slots__ = ("_coro", "_loop", "_task", "_capture", "_on_complete")

    def __init__(
        self,
        coro: Coroutine[Any, Any, Any],
        yielded: object,
        loop: asyncio.AbstractEventLoop,
        task: SchedulerTask,
        capture: CallSoonCapture,
        on_complete: Callable[[], None] | None = None,
    ) -> None:
        self._coro: Coroutine[Any, Any, Any] | None = coro
        self._loop = loop
        self._task = task
        self._capture = capture
        self._on_complete = on_complete
        self._attach(yielded)

    def _attach(self, yielded: object) -> None:
        """Attach to a yielded value to resume when ready."""
        if yielded is None:
            self._loop.call_soon(self._step)
        elif hasattr(yielded, "add_done_callback"):
            self._task._waiter = yielded  # type: ignore[assignment, ty:invalid-assignment]
            yielded.add_done_callback(self._on_future_done)  # type: ignore[union-attr, ty:call-non-callable]
        else:
            self._finish()

    def _on_future_done(self, future: asyncio.Future[Any]) -> None:
        self._task._waiter = None
        if asyncio.current_task() is not None:
            self._loop.call_soon(self._step)
            return
        self._step()

    def _step(self) -> None:
        if self._coro is None:
            return

        if self._task._cancel_flag:
            self._task._cancel_flag = False
            _enter_task(self._loop, self._task)
            try:
                yielded = self._coro.throw(
                    asyncio.CancelledError(self._task._cancel_msg)
                )
            except (StopIteration, BaseException):
                _leave_task(self._loop, self._task)
                self._finish()
                return
            _leave_task(self._loop, self._task)
            self._attach(yielded)
            return

        self._capture.enter()
        result = drive_inline(self._coro, self._task, self._loop, self._capture)
        self._capture.leave()

        if isinstance(result, Completed):
            self._finish()
        elif isinstance(result, Failed):
            self._finish()
        elif isinstance(result, Suspended):
            self._attach(result.yielded)

    def _finish(self) -> None:
        self._coro = None
        self._task._waiter = None
        if self._on_complete is not None:
            self._on_complete()
