"""Zero-GIL dispatch loop with inline coroutine driving.

Installs an fd-based wakeup on the asyncio event loop.  Drains
requests from the Rust crossbeam channel and drives each ASGI
coroutine inline.  Simple handlers complete in ~21us with zero
event loop scheduling.  Handlers that suspend on real I/O fall
back to callback-based continuation.

Called once from Rust during reactor init via
``py.import(c"apx._dispatch")?.call_method1(c"install_dispatch", ...)``.
"""

from __future__ import annotations

import asyncio
import os
import traceback
from collections.abc import Coroutine
from typing import Any, Callable

from apx._continuation import Continuation
from apx._core import RequestQueue
from apx._scheduler import (
    CallSoonCapture,
    Completed,
    Failed,
    SchedulerTask,
    Suspended,
    drive_inline,
)


def install_dispatch(
    loop: asyncio.AbstractEventLoop,
    queue: RequestQueue,
    app: Callable[..., Coroutine[Any, Any, None]],
    wakeup_fd: int | None = None,
) -> None:
    """Install the inline dispatch driver on the asyncio event loop."""

    max_drain_batch: int = 8
    capture = CallSoonCapture(loop)

    async def _guarded(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        try:
            await app(scope, receive, send)
        except Exception as exc:
            tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
            send.send_error(tb)

    def _dispatch_inline(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        """Drive one request inline.  Falls back on suspension."""
        coro = _guarded(scope, receive, send)
        try:
            task = SchedulerTask(loop=loop)

            capture.enter()
            result = drive_inline(coro, task, loop, capture)
            capture.leave()
        except BaseException:
            coro.close()
            raise

        if isinstance(result, Completed):
            return
        elif isinstance(result, Failed):
            tb = "".join(
                traceback.format_exception(
                    type(result.exc), result.exc, result.exc.__traceback__
                )
            )
            send.send_error(tb)
            return
        elif isinstance(result, Suspended):
            Continuation(coro, result.yielded, loop, task, capture)

    def _drain_queue() -> None:
        for _ in range(max_drain_batch):
            result: tuple[Any, Any, Any] | None = queue.try_recv()
            if result is None:
                return
            scope, receive, send = result
            _dispatch_inline(scope, receive, send)
        loop.call_soon(_drain_queue)

    if wakeup_fd is not None:

        def _on_readable() -> None:
            try:
                os.read(wakeup_fd, 4096)
            except BlockingIOError:
                pass
            _drain_queue()

        loop.call_soon_threadsafe(loop.add_reader, wakeup_fd, _on_readable)
    else:
        install_dispatch._drain_queue = _drain_queue  # type: ignore[attr-defined, ty:unresolved-attribute]
