"""Zero-GIL dispatch loop for the 3-thread architecture.

Installs an fd-based wakeup on the asyncio event loop (Unix) or
exposes a drain callback for ``call_soon_threadsafe`` (Windows).

Called once from Rust during reactor init via
``py.import(c"apx._dispatch")?.call_method1(c"install_dispatch", ...)``.
"""

from __future__ import annotations

import asyncio
import os
import traceback
from collections.abc import Coroutine
from typing import Any, Callable

from apx._core import RequestQueue


def install_dispatch(
    loop: asyncio.AbstractEventLoop,
    queue: RequestQueue,
    app: Callable[..., Coroutine[Any, Any, None]],
    wakeup_fd: int | None = None,
) -> None:
    """Install the zero-GIL dispatch reader on the asyncio event loop.

    On Unix: registers ``wakeup_fd`` with the loop's selector via ``add_reader``.
    On Windows: ``wakeup_fd`` is ``None`` — Rust uses ``call_soon_threadsafe``
    which appends ``_drain_queue`` directly to ``_ready`` (no fd needed).
    """

    async def _guarded(scope: Any, receive: Any, send: Any) -> None:
        try:
            await app(scope, receive, send)
        except Exception as exc:
            tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
            send.send_error(tb)

    def _drain_queue() -> None:
        while True:
            result = queue.try_recv()
            if result is None:
                break
            scope, receive, send = result
            loop.create_task(_guarded(scope, receive, send))

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
