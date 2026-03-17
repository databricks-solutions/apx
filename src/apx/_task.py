"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler."""
from __future__ import annotations

import asyncio


async def _sentinel() -> None:
    """Complete immediately — Task.__init__ needs a real coroutine for C struct init."""


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for asyncio.current_task() during driving.

    Calls super().__init__() with a completed sentinel to properly init
    the C struct fields. The real coroutine is driven by the Rust scheduler.

    Task.__init__ auto-schedules ``call_soon(self.__step)``. We cancel that
    handle immediately to prevent __step's ``_enter_task`` from conflicting
    with the Rust driver's own ``_enter_task`` call.
    """

    def __init__(
        self, coro: object, *, loop: asyncio.AbstractEventLoop | None = None
    ) -> None:
        if loop is None:
            loop = asyncio.get_running_loop()
        sentinel = _sentinel()
        n_before = len(loop._ready)
        super().__init__(sentinel, loop=loop)
        # Cancel the __step handle that Task.__init__ auto-scheduled.
        # We hold the GIL, so nothing else mutates _ready between the
        # super().__init__() call and here.
        if len(loop._ready) > n_before:
            loop._ready[-1].cancel()
        # Close the sentinel coroutine to avoid "was never awaited" warnings.
        sentinel.close()
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
