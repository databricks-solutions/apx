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
    keeping one task entered per loop at a time.  Runs entirely on the asyncio thread.

    When an asyncio Future resolves, the continuation delivers the
    result (or exception) to the coroutine via ``drive_inline``'s
    ``send_value`` / ``send_exception`` parameters — matching the
    standard ``Task.__step`` protocol.
    """

    __slots__ = (
        "_coro",
        "_loop",
        "_task",
        "_capture",
        "_on_complete",
        "_resolved_future",
    )

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
        self._resolved_future: asyncio.Future[Any] | None = None
        self._attach(yielded)

    def _attach(self, yielded: object) -> None:
        """Attach to a yielded value to resume when ready."""
        if yielded is None:
            # yield None (e.g. asyncio.sleep(0)) — re-enter next cycle.
            self._loop.call_soon(self._step)
        elif hasattr(yielded, "add_done_callback"):
            # asyncio.Future — resume when I/O completes.
            self._task._waiter = yielded  # type: ignore[assignment, ty:invalid-assignment]
            yielded.add_done_callback(self._on_future_done)  # type: ignore[union-attr, ty:call-non-callable]
        else:
            self._finish()

    def _on_future_done(self, future: asyncio.Future[Any]) -> None:
        self._task._waiter = None
        self._resolved_future = future
        # If another task is currently entered (defensive guard),
        # defer to next callback cycle.
        if asyncio.current_task() is not None:
            self._loop.call_soon(self._step)
            return
        self._step()

    def _extract_resume(
        self,
    ) -> tuple[Any, BaseException | None]:
        """Extract send_value / send_exception from a resolved Future.

        Mirrors the ``Task.__step`` protocol: deliver the Future's
        result to the coroutine, or throw its exception.
        """
        future = self._resolved_future
        self._resolved_future = None
        if future is None:
            # yield-None re-entry — no value to deliver.
            return None, None
        if future.cancelled():
            return None, asyncio.CancelledError()
        exc = future.exception()
        if exc is not None:
            return None, exc
        return future.result(), None

    def _step(self) -> None:
        if self._coro is None:
            return

        # Check cancellation flag (asyncio.timeout / anyio.fail_after).
        if self._task._cancel_flag:
            self._task._cancel_flag = False
            _enter_task(self._loop, self._task)
            try:
                yielded = self._coro.throw(
                    asyncio.CancelledError(self._task._cancel_msg)
                )
            except StopIteration:
                # Coroutine caught CancelledError and returned normally.
                _leave_task(self._loop, self._task)
                self._finish()
                return
            except asyncio.CancelledError:
                # Coroutine re-raised CancelledError — expected.
                _leave_task(self._loop, self._task)
                self._finish()
                return
            except BaseException as exc:
                # Coroutine raised a different exception during cancel
                # cleanup (e.g. error in a yield-dep finalizer).
                _leave_task(self._loop, self._task)
                _log_cancel_exception(exc)
                self._finish()
                return
            _leave_task(self._loop, self._task)
            self._attach(yielded)
            return

        # Normal step: deliver the Future result and resume driving.
        send_value, send_exception = self._extract_resume()

        self._capture.enter()
        result = drive_inline(
            self._coro,
            self._task,
            self._loop,
            self._capture,
            send_value=send_value,
            send_exception=send_exception,
        )
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
        self._resolved_future = None
        if self._on_complete is not None:
            self._on_complete()


def _log_cancel_exception(exc: BaseException) -> None:
    """Log an unexpected exception from the cancel path.

    Avoids silent swallowing — if cleanup code (e.g. a yield-dep
    finalizer) raises during cancellation, it shows up in logs.
    """
    import logging
    import traceback

    tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
    logging.getLogger("apx.dispatch").warning(
        "exception during cancellation cleanup:\n%s", tb
    )
