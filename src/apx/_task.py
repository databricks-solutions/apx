"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler."""
from __future__ import annotations

import asyncio


async def _sentinel() -> None:
    """Suspend forever — the Rust scheduler drives the real coroutine."""
    await asyncio.get_running_loop().create_future()


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for asyncio.current_task() during driving.

    Calls super().__init__() with a suspended sentinel to properly init
    the C struct fields. The real coroutine is driven by the Rust scheduler.
    """

    def __init__(
        self, coro: object, *, loop: asyncio.AbstractEventLoop | None = None
    ) -> None:
        super().__init__(_sentinel(), loop=loop)
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
