"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler."""
from __future__ import annotations

import asyncio


async def _sentinel() -> None:
    """Complete immediately — Task.__init__ needs a real coroutine for C struct init."""


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for asyncio.current_task() during driving.

    The sentinel coroutine completes immediately when __step runs on the
    reactor thread. No private-API cancellation — works on CPython, uvloop,
    and any future event loop implementation.
    """

    def __init__(
        self,
        coro: object,
        *,
        loop: asyncio.AbstractEventLoop | None = None,
        **kwargs: object,
    ) -> None:
        if loop is None:
            loop = asyncio.get_running_loop()
        # Sentinel completes atomically: enter → send(None) → StopIteration → leave.
        # No need to cancel __step — the cost is ~1μs and it's harmless.
        super().__init__(_sentinel(), loop=loop, **kwargs)
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
