"""Unit tests for the inline dispatch driver.

Tests cover:
- Batch drain stops after ``max_drain_batch`` items and re-schedules
- Batch drain exhausts small queues without re-scheduling
- ``_guarded`` calls ``send.send_error()`` on app exceptions
- Inline driving completes simple coroutines without tasks
- ``CallSoonCapture`` intercepts and flushes callbacks correctly
"""

from __future__ import annotations

import asyncio
import traceback
from collections.abc import Coroutine
from typing import Any, Callable
from unittest.mock import AsyncMock, MagicMock

from apx._continuation import Continuation
from apx._scheduler import (
    CallSoonCapture,
    Completed,
    Failed,
    SchedulerTask,
    Suspended,
    drive_inline,
)


# ---------------------------------------------------------------------------
# Helpers — replicate the dispatch wiring without the Rust native module.
# ---------------------------------------------------------------------------


def _make_dispatch(
    queue_items: list[tuple[Any, Any, Any] | None],
    app: Callable[..., Coroutine[Any, Any, None]] | None = None,
    max_drain_batch: int = 8,
) -> tuple[asyncio.AbstractEventLoop, Callable[[], None], MagicMock]:
    """Build an inline dispatch drain with a mock queue.

    Returns ``(loop, _drain_queue, mock_queue)`` so callers can invoke
    ``_drain_queue()`` and inspect what was dispatched.
    """
    items = list(queue_items)

    mock_queue = MagicMock()
    mock_queue.try_recv.side_effect = lambda: items.pop(0) if items else None

    loop = asyncio.new_event_loop()
    dispatched_count: list[int] = [0]

    call_soon_calls: list[Any] = []
    original_call_soon = loop.call_soon

    def tracking_call_soon(cb: Any, *args: Any, **kwargs: Any) -> Any:
        call_soon_calls.append((cb, args))
        return original_call_soon(cb, *args, **kwargs)

    loop.call_soon = tracking_call_soon  # type: ignore[assignment]

    capture = CallSoonCapture(loop)

    if app is None:
        app = AsyncMock()

    async def _guarded(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        try:
            await app(scope, receive, send)  # type: ignore[misc]
        except Exception as exc:
            tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
            send.send_error(tb)

    def _dispatch_inline(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        coro = _guarded(scope, receive, send)
        task = SchedulerTask(loop=loop)

        capture.enter()
        result = drive_inline(coro, task, loop, capture)
        capture.leave()

        dispatched_count[0] += 1
        if isinstance(result, Completed):
            return
        elif isinstance(result, Failed):
            tb_str = "".join(
                traceback.format_exception(
                    type(result.exc), result.exc, result.exc.__traceback__
                )
            )
            send.send_error(tb_str)
        elif isinstance(result, Suspended):
            Continuation(coro, result.yielded, loop, task, capture)

    def _drain_queue() -> None:
        for _ in range(max_drain_batch):
            result: tuple[Any, Any, Any] | None = mock_queue.try_recv()
            if result is None:
                return
            scope, receive, send = result
            _dispatch_inline(scope, receive, send)
        loop.call_soon(_drain_queue)

    _drain_queue._dispatched_count = dispatched_count  # type: ignore[attr-defined]
    _drain_queue._call_soon_calls = call_soon_calls  # type: ignore[attr-defined]

    return loop, _drain_queue, mock_queue


def _make_item(
    scope: Any = None,
    receive: Any = None,
    send: Any = None,
) -> tuple[Any, Any, Any]:
    """Create a ``(scope, receive, send)`` tuple for the mock queue."""
    return (scope or {}, receive or MagicMock(), send or MagicMock())


# ---------------------------------------------------------------------------
# Tests — batch drain limits
# ---------------------------------------------------------------------------


class TestDrainBatchLimit:
    """_drain_queue stops after max_drain_batch and re-schedules."""

    def test_batch_limit_triggers_reschedule(self) -> None:
        """When the queue has more items than the batch size, ``call_soon``
        is used to re-schedule the drain, yielding to the event loop."""
        items = [_make_item() for _ in range(12)]
        loop, drain, mock_q = _make_dispatch(items, max_drain_batch=8)
        try:
            drain()

            assert drain._dispatched_count[0] == 8  # type: ignore[attr-defined]
            assert any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            loop.close()

    def test_small_queue_no_reschedule(self) -> None:
        """When the queue has fewer items than the batch size, no
        ``call_soon`` re-schedule occurs."""
        items = [_make_item() for _ in range(3)]
        loop, drain, mock_q = _make_dispatch(items, max_drain_batch=8)
        try:
            drain()

            assert drain._dispatched_count[0] == 3  # type: ignore[attr-defined]
            assert not any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            loop.close()

    def test_empty_queue_noop(self) -> None:
        """Draining an empty queue returns immediately."""
        loop, drain, mock_q = _make_dispatch([], max_drain_batch=8)
        try:
            drain()

            assert drain._dispatched_count[0] == 0  # type: ignore[attr-defined]
            mock_q.try_recv.assert_called_once()
        finally:
            loop.close()

    def test_exact_batch_size_triggers_reschedule(self) -> None:
        """When the queue has exactly batch-size items, the drain cannot
        know the queue is empty without a 9th ``try_recv``, so it
        re-schedules conservatively."""
        items = [_make_item() for _ in range(8)]
        loop, drain, mock_q = _make_dispatch(items, max_drain_batch=8)
        try:
            drain()

            assert drain._dispatched_count[0] == 8  # type: ignore[attr-defined]
            assert any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            loop.close()


# ---------------------------------------------------------------------------
# Tests — error handling
# ---------------------------------------------------------------------------


class TestGuarded:
    """_guarded handles app errors correctly."""

    def test_app_error_calls_send_error(self) -> None:
        """If the ASGI app raises, ``send.send_error(tb)`` is called
        inline — no event loop ticking required."""
        mock_send = MagicMock()
        item = _make_item(send=mock_send)
        app = AsyncMock(side_effect=ValueError("handler failed"))
        loop, drain, _ = _make_dispatch([item], app=app)
        try:
            drain()

            mock_send.send_error.assert_called_once()
            tb_arg: str = mock_send.send_error.call_args[0][0]
            assert "handler failed" in tb_arg
        finally:
            loop.close()

    def test_successful_app_call(self) -> None:
        """Happy path: app runs with scope/receive/send, no errors."""
        mock_scope: dict[str, Any] = {"type": "http"}
        mock_receive = MagicMock()
        mock_send = MagicMock()
        item = _make_item(scope=mock_scope, receive=mock_receive, send=mock_send)
        app = AsyncMock()
        loop, drain, _ = _make_dispatch([item], app=app)
        try:
            drain()

            app.assert_called_once_with(mock_scope, mock_receive, mock_send)
            mock_send.send_error.assert_not_called()
        finally:
            loop.close()


# ---------------------------------------------------------------------------
# Tests — inline driving
# ---------------------------------------------------------------------------


class TestInlineDriving:
    """drive_inline completes simple coroutines without creating tasks."""

    def test_sync_completing_coroutine_returns_completed(self) -> None:
        """A coroutine that finishes without yielding returns Completed."""
        loop = asyncio.new_event_loop()
        capture = CallSoonCapture(loop)
        try:

            async def simple() -> None:
                pass

            task = SchedulerTask(loop=loop)
            capture.enter()
            result = drive_inline(simple(), task, loop, capture)
            capture.leave()

            assert isinstance(result, Completed)
        finally:
            loop.close()

    def test_coroutine_with_return_value_completes(self) -> None:
        """A coroutine that returns a value still results in Completed."""
        loop = asyncio.new_event_loop()
        capture = CallSoonCapture(loop)
        try:

            async def with_return() -> str:
                return "hello"

            task = SchedulerTask(loop=loop)
            capture.enter()
            result = drive_inline(with_return(), task, loop, capture)
            capture.leave()

            assert isinstance(result, Completed)
        finally:
            loop.close()

    def test_exception_returns_failed(self) -> None:
        """A coroutine that raises an exception returns Failed."""
        loop = asyncio.new_event_loop()
        capture = CallSoonCapture(loop)
        try:

            async def raises() -> None:
                raise RuntimeError("boom")

            task = SchedulerTask(loop=loop)
            capture.enter()
            result = drive_inline(raises(), task, loop, capture)
            capture.leave()

            assert isinstance(result, Failed)
            assert isinstance(result.exc, RuntimeError)
            assert "boom" in str(result.exc)
        finally:
            loop.close()

    def test_real_future_returns_suspended(self) -> None:
        """A coroutine awaiting a real asyncio Future returns Suspended."""
        loop = asyncio.new_event_loop()
        capture = CallSoonCapture(loop)
        try:
            fut = loop.create_future()

            async def waits_on_future() -> None:
                await fut

            task = SchedulerTask(loop=loop)
            capture.enter()
            result = drive_inline(waits_on_future(), task, loop, capture)
            capture.leave()

            assert isinstance(result, Suspended)
            assert result.yielded is not None
        finally:
            loop.close()

    def test_multiple_items_complete_inline(self) -> None:
        """Multiple queue items driven inline complete synchronously."""
        items = [_make_item() for _ in range(5)]
        app = AsyncMock()
        loop, drain, _ = _make_dispatch(items, app=app, max_drain_batch=8)
        try:
            drain()

            assert drain._dispatched_count[0] == 5  # type: ignore[attr-defined]
            assert app.call_count == 5
        finally:
            loop.close()


# ---------------------------------------------------------------------------
# Tests — CallSoonCapture
# ---------------------------------------------------------------------------


class TestCallSoonCapture:
    """CallSoonCapture intercepts and processes callbacks correctly."""

    def test_active_captures_callbacks(self) -> None:
        """When active, call_soon callbacks are captured, not executed."""
        loop = asyncio.new_event_loop()
        try:
            capture = CallSoonCapture(loop)
            called: list[str] = []

            capture.enter()
            loop.call_soon(lambda: called.append("should_be_captured"))
            assert len(called) == 0

            capture.leave()
        finally:
            loop.close()

    def test_inactive_passes_through(self) -> None:
        """When not active, call_soon delegates to the original."""
        loop = asyncio.new_event_loop()
        try:
            scheduled: list[Any] = []
            original = loop.call_soon

            def tracking(cb: Any, *args: Any, **kw: Any) -> Any:
                scheduled.append(cb)
                return original(cb, *args, **kw)

            loop.call_soon = tracking  # type: ignore[assignment]
            capture = CallSoonCapture(loop)

            callback = lambda: None  # noqa: E731
            loop.call_soon(callback)
            assert callback in scheduled
        finally:
            loop.close()

    def test_flush_processes_captured(self) -> None:
        """flush() runs captured callbacks inline."""
        loop = asyncio.new_event_loop()
        try:
            capture = CallSoonCapture(loop)
            called: list[str] = []

            capture.enter()
            loop.call_soon(lambda: called.append("a"))
            loop.call_soon(lambda: called.append("b"))

            capture.flush()
            assert called == ["a", "b"]

            capture.leave()
        finally:
            loop.close()

    def test_leave_spills_remaining(self) -> None:
        """leave() schedules remaining callbacks via the real call_soon."""
        loop = asyncio.new_event_loop()
        try:
            scheduled: list[Any] = []
            original = loop.call_soon

            def tracking(cb: Any, *args: Any, **kw: Any) -> Any:
                scheduled.append(cb)
                return original(cb, *args, **kw)

            loop.call_soon = tracking  # type: ignore[assignment]
            capture = CallSoonCapture(loop)

            callback = lambda: None  # noqa: E731
            capture.enter()
            loop.call_soon(callback)

            capture.leave()
            assert callback in scheduled
        finally:
            loop.close()
