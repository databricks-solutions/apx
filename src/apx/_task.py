"""Scheduler task proxy — a real asyncio.Task subclass for the Rust scheduler.

Per-request ``_SchedulerTask`` wrapping a no-op ``_sentinel()`` coroutine.
The user's actual coroutine is driven directly by the Rust scheduler via
``PyIter_Send`` — the ``_SchedulerTask`` exists only to satisfy
``asyncio.current_task()`` and ``_enter_task``/``_leave_task``.

- **Python 3.12+:** ``eager_start=True`` completes the sentinel inline
  during ``__init__`` (via ``_swap_current_task``, not ``_enter_task``).
  No ``__step`` callback ever reaches ``_ready``.

- **Python 3.11:** ``eager_start`` is unavailable, so ``Task.__init__``
  schedules a ``__step`` callback via ``call_soon``.  We immediately
  cancel that ``__step`` handle from ``_ready`` (CPython only) to prevent
  A5 ``_enter_task`` collisions under concurrent load.  On uvloop the
  sentinel completes atomically in libuv's callback queue.
"""
from __future__ import annotations

import asyncio
import sys

_PY312 = sys.version_info >= (3, 12)


async def _sentinel() -> None:
    """Complete immediately — Task.__init__ needs a real coroutine for C struct init."""


class _SchedulerTask(asyncio.Task):
    """Per-request proxy: wraps _sentinel(), stores the real coro for display."""

    def __init__(
        self,
        coro: object,
        *,
        loop: asyncio.AbstractEventLoop | None = None,
        **kwargs: object,
    ) -> None:
        if loop is None:
            loop = asyncio.get_running_loop()
        init_kwargs: dict[str, object] = {"loop": loop}
        if _PY312:
            init_kwargs["eager_start"] = True
        init_kwargs.update(kwargs)
        ready = getattr(loop, "_ready", None)
        n_before = len(ready) if ready is not None else 0
        super().__init__(_sentinel(), **init_kwargs)
        if not _PY312 and ready is not None and len(ready) > n_before:
            ready.pop()
        self._real_coro = coro
        self._log_destroy_pending = False

    def get_coro(self) -> object:
        return self._real_coro
