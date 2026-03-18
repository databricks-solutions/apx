"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler."""
from __future__ import annotations

import asyncio
import sys

_EAGER_START = sys.version_info >= (3, 12)


async def _sentinel() -> None:
    """Complete immediately — Task.__init__ needs a real coroutine for C struct init."""


class _SchedulerTask(asyncio.Task):
    """Lightweight Task stand-in for asyncio.current_task() during driving.

    The sentinel coroutine completes immediately when __step runs on the
    reactor thread. No private-API cancellation — works on CPython, uvloop,
    and any future event loop implementation.

    On Python 3.12+, ``eager_start=True`` causes the sentinel to complete
    inline during ``__init__`` via ``__eager_start`` (uses ``_swap_current_task``,
    not ``_enter_task`` — no collision check). No ``__step`` callback ever
    reaches ``_ready``, eliminating the dominant I1/A5 collision source.
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
        if _EAGER_START:
            super().__init__(_sentinel(), loop=loop, eager_start=True, **kwargs)
        else:
            super().__init__(_sentinel(), loop=loop, **kwargs)
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
