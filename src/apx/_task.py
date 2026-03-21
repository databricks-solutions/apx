"""Lightweight asyncio.Task stand-in for inline coroutine driving.

The Rust scheduler drives ASGI coroutines inline (via ``coro.send(None)``)
rather than through ``loop.create_task``.  asyncio libraries (sniffio, anyio,
Starlette) expect ``asyncio.current_task()`` to return a real ``Task`` during
handler execution.  ``_SmartSchedulerTask`` satisfies that contract without
ever being *stepped* by the event loop itself — the real coroutine is driven
externally by the dispatch loop.

The sentinel coroutine passed to ``Task.__init__`` never completes, keeping
``done() == False`` so the task appears "running" to any code that inspects it.
On CPython 3.11 the sentinel's ``__step`` callback is physically removed from
``loop._ready`` to prevent ``_enter_task`` collisions under concurrency (see
asyncio skill §A5 — sentinel ``__step`` dominant collision source).
On CPython 3.12+ ``eager_start`` is available; the sentinel completes inline
during ``__init__`` so no ``__step`` callback ever reaches ``_ready``.

Cancel / uncancel bookkeeping is forwarded so that ``asyncio.timeout`` and
``TaskGroup._abort`` work correctly when the handler is driven inline.
"""

from __future__ import annotations

import asyncio
import asyncio.tasks
import sys
from typing import Any

_PY_312_PLUS: bool = sys.version_info >= (3, 12)


async def _sentinel() -> None:
    """Suspend forever — the real coroutine is driven externally.

    Creates a Future that is never resolved, so the sentinel awaits
    indefinitely.  This keeps ``Task.done() == False`` and ensures
    ``__step`` only runs once (the initial schedule from ``__init__``).
    """
    await asyncio.get_running_loop().create_future()


class _SmartSchedulerTask(asyncio.Task):
    """Task subclass with cancel forwarding for timeout / TaskGroup.

    Never-completing sentinel keeps ``done() == False``.
    Overrides ``cancel`` / ``cancelling`` / ``uncancel`` so that nested
    timeout scopes and TaskGroup cancellation propagate correctly to
    any waiter Future parked by the inline ministepper fallback path.

    On 3.11 the sentinel ``__step`` handle is popped from ``_ready``
    immediately after ``super().__init__`` to eliminate the dominant
    ``_enter_task`` collision source (see asyncio skill §A5).
    On 3.12+ ``eager_start`` eliminates the callback entirely.
    """

    __slots__ = (
        "_cancel_count",
        "_cancel_flag",
        "_cancel_msg",
        "_waiter",
    )

    _cancel_flag: bool
    _cancel_msg: str | None
    _cancel_count: int
    _waiter: asyncio.Future[Any] | None

    def __init__(self, *, loop: asyncio.AbstractEventLoop) -> None:
        if _PY_312_PLUS:
            super().__init__(_sentinel(), loop=loop, eager_start=True)  # type: ignore[call-arg]
        else:
            super().__init__(_sentinel(), loop=loop)
            # Physically remove the sentinel __step from _ready so it cannot
            # fire _enter_task on the asyncio thread and collide with our
            # inline _enter_task on the dispatch thread.  Handle.cancel() is
            # NOT sufficient — cancelled handles remain in the deque and still
            # cause collisions under high concurrency.
            ready = getattr(loop, "_ready", None)
            if ready is not None:
                try:
                    ready.pop()
                except IndexError:
                    pass

        self._log_destroy_pending = False
        self._cancel_flag = False
        self._cancel_msg = None
        self._cancel_count = 0
        self._waiter = None

    # -- cancel forwarding ---------------------------------------------------

    def cancel(self, msg: str | None = None) -> bool:
        """Accept cancellation and forward to the parked waiter if any.

        Returns ``True`` unconditionally (standard ``Task.cancel`` contract)
        so that ``TaskGroup._abort`` considers the cancel delivered.
        """
        self._cancel_flag = True
        self._cancel_msg = msg
        self._cancel_count += 1
        if self._waiter is not None and not self._waiter.done():
            self._waiter.cancel(msg=msg)
        return True

    def cancelling(self) -> int:
        """Number of pending cancel requests (for nested timeout support)."""
        return self._cancel_count

    def uncancel(self) -> int:
        """Decrement cancel counter (called by timeout scope on exit)."""
        self._cancel_count = max(0, self._cancel_count - 1)
        return self._cancel_count
