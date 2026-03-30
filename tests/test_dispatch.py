"""Unit tests for the batch-limited drain.

Tests cover:
- Batch drain stops after ``max_drain_batch`` items and re-schedules
- Batch drain exhausts small queues without re-scheduling
- ``_guarded`` calls ``send.send_error()`` on app exceptions
"""

from __future__ import annotations

import asyncio
from collections.abc import Coroutine
from typing import Any, Callable
from unittest.mock import AsyncMock, MagicMock


def _cancel_all(
    loop: asyncio.AbstractEventLoop, tasks: list[asyncio.Task[None]]
) -> None:
    """Cancel and await all tasks so the loop can close cleanly."""
    for t in tasks:
        t.cancel()
    if tasks:
        loop.run_until_complete(asyncio.gather(*tasks, return_exceptions=True))


# ---------------------------------------------------------------------------
# Helpers — replicate the dispatch wiring without the Rust native module.
# ---------------------------------------------------------------------------

def _make_dispatch(
    queue_items: list[tuple[Any, Any, Any] | None],
    app: Callable[..., Coroutine[Any, Any, None]] | None = None,
    max_drain_batch: int = 8,
) -> tuple[asyncio.AbstractEventLoop, Callable[[], None], MagicMock]:
    """Build an ``install_dispatch``-style closure with a mock queue.

    Returns ``(loop, _drain_queue, mock_queue)`` so callers can invoke
    ``_drain_queue()`` and inspect what was scheduled.
    """
    items = list(queue_items)

    mock_queue = MagicMock()
    mock_queue.try_recv.side_effect = lambda: items.pop(0) if items else None

    loop = asyncio.new_event_loop()
    tasks_created: list[asyncio.Task[None]] = []

    original_create_task = loop.create_task

    def tracking_create_task(coro: Any, **kwargs: Any) -> Any:
        task = original_create_task(coro, **kwargs)
        tasks_created.append(task)
        return task

    loop.create_task = tracking_create_task  # type: ignore[assignment]

    if app is None:
        app = AsyncMock()

    call_soon_calls: list[Any] = []
    original_call_soon = loop.call_soon

    def tracking_call_soon(cb: Any, *args: Any, **kwargs: Any) -> Any:
        call_soon_calls.append((cb, args))
        return original_call_soon(cb, *args, **kwargs)

    loop.call_soon = tracking_call_soon  # type: ignore[assignment]

    import traceback

    async def _guarded(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        try:
            await app(scope, receive, send)  # type: ignore[misc]
        except Exception as exc:
            tb = "".join(
                traceback.format_exception(type(exc), exc, exc.__traceback__)
            )
            send.send_error(tb)

    def _drain_queue() -> None:
        for _ in range(max_drain_batch):
            result: tuple[Any, Any, Any] | None = mock_queue.try_recv()
            if result is None:
                return
            scope, receive, send = result
            loop.create_task(_guarded(scope, receive, send))
        loop.call_soon(_drain_queue)

    _drain_queue._tasks_created = tasks_created  # type: ignore[attr-defined]
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
# Tests
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

            assert len(drain._tasks_created) == 8  # type: ignore[attr-defined]
            assert any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
            loop.close()

    def test_small_queue_no_reschedule(self) -> None:
        """When the queue has fewer items than the batch size, no
        ``call_soon`` re-schedule occurs."""
        items = [_make_item() for _ in range(3)]
        loop, drain, mock_q = _make_dispatch(items, max_drain_batch=8)
        try:
            drain()

            assert len(drain._tasks_created) == 3  # type: ignore[attr-defined]
            assert not any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
            loop.close()

    def test_empty_queue_noop(self) -> None:
        """Draining an empty queue returns immediately."""
        loop, drain, mock_q = _make_dispatch([], max_drain_batch=8)
        try:
            drain()

            assert len(drain._tasks_created) == 0  # type: ignore[attr-defined]
            mock_q.try_recv.assert_called_once()
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
            loop.close()

    def test_exact_batch_size_triggers_reschedule(self) -> None:
        """When the queue has exactly batch-size items, the drain cannot
        know the queue is empty without a 9th ``try_recv``, so it
        re-schedules conservatively."""
        items = [_make_item() for _ in range(8)]
        loop, drain, mock_q = _make_dispatch(items, max_drain_batch=8)
        try:
            drain()

            assert len(drain._tasks_created) == 8  # type: ignore[attr-defined]
            assert any(
                cb is drain
                for cb, _ in drain._call_soon_calls  # type: ignore[attr-defined]
            )
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
            loop.close()


class TestGuarded:
    """_guarded handles app errors correctly."""

    def test_app_error_calls_send_error(self) -> None:
        """If the ASGI app raises, ``send.send_error(tb)`` is called."""
        mock_send = MagicMock()
        item = _make_item(send=mock_send)
        app = AsyncMock(side_effect=ValueError("handler failed"))
        loop, drain, _ = _make_dispatch([item], app=app)
        try:
            drain()
            loop.run_until_complete(asyncio.sleep(0))
            loop.run_until_complete(asyncio.sleep(0))

            mock_send.send_error.assert_called_once()
            tb_arg: str = mock_send.send_error.call_args[0][0]
            assert "handler failed" in tb_arg
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
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
            loop.run_until_complete(asyncio.sleep(0))
            loop.run_until_complete(asyncio.sleep(0))

            app.assert_called_once_with(mock_scope, mock_receive, mock_send)
            mock_send.send_error.assert_not_called()
        finally:
            _cancel_all(loop, drain._tasks_created)  # type: ignore[attr-defined]
            loop.close()
