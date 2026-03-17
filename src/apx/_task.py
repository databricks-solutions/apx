"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler."""
from __future__ import annotations

import asyncio


async def _sentinel() -> None:
    """Complete immediately — Task.__init__ needs a real coroutine for C struct init."""


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for asyncio.current_task() during driving.

    Calls super().__init__() with an immediately-completing sentinel to
    properly init the C struct fields. The real coroutine is driven by the
    Rust scheduler.

    Task.__init__ auto-schedules ``call_soon(self.__step)``. On CPython's
    asyncio we cancel that handle via ``loop._ready``; on uvloop (which
    doesn't expose ``_ready``) we let __step run — the empty sentinel
    completes atomically (enter → complete → leave) with no side effects.
    """

    def __init__(
        self, coro: object, *, loop: asyncio.AbstractEventLoop | None = None
    ) -> None:
        if loop is None:
            loop = asyncio.get_running_loop()
        sentinel = _sentinel()
        # _ready is CPython's asyncio callback deque; uvloop doesn't have it.
        ready = getattr(loop, "_ready", None)
        n_before = len(ready) if ready is not None else 0
        super().__init__(sentinel, loop=loop)
        # Cancel the __step handle that Task.__init__ auto-scheduled.
        # On uvloop this is a no-op — __step will run the empty sentinel
        # atomically (enter/complete/leave) which is harmless.
        if ready is not None and len(ready) > n_before:
            ready[-1].cancel()
            # Sentinel won't be consumed by __step; close to suppress
            # "coroutine was never awaited" warning.
            sentinel.close()
        # On uvloop: don't close — let __step consume the sentinel normally.
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
