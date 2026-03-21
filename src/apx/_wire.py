"""``call_soon`` wire for inline coroutine driving.

During inline mode (inside ``_drain_queue``), the dispatch loop drives
coroutines via direct ``coro.send(None)`` instead of ``loop.create_task``.
Any code that the coroutine executes may call ``loop.call_soon`` — for
example ``Task.__init__`` schedules a ``__step`` callback, and user code
may schedule work via ``loop.call_soon`` directly.

If those callbacks land in ``loop._ready`` while we are between
``_enter_task`` / ``_leave_task`` on the dispatch thread, the asyncio
event loop thread would step them and call ``_enter_task`` for a
*different* task — triggering a collision (``RuntimeError: task is being
executed``).

The wire solves this by monkey-patching ``loop.call_soon`` while inline
mode is active.  Captured callbacks are stored in a thread-local deque
and replayed after ``_leave_task`` / ``leave_inline``, when
``current_task() is None`` — a safe window with no collision risk.

Thread-local storage is used because the dispatch loop runs exclusively
on Thread 2 (the asyncio thread) and no other thread calls these
functions.
"""

from __future__ import annotations
from typing import TypeVar

import asyncio
import collections
import contextvars
import threading
from collections.abc import Callable

# ---------------------------------------------------------------------------
# Domain types
# ---------------------------------------------------------------------------

# A single deferred ``call_soon`` entry: the callback, its positional
# arguments, and the optional ``contextvars.Context`` it should execute in.
# Stored in the deferred queue when the wire intercepts a ``call_soon``
# during inline driving mode.
DeferredCallback = tuple[
    Callable[..., object],
    tuple[object, ...],
    contextvars.Context | None,
]

# Signature of the original ``loop.call_soon`` that the wire wraps.
OriginalCallSoon = Callable[..., asyncio.Handle]

# ---------------------------------------------------------------------------
# Dispatch-thread state
# ---------------------------------------------------------------------------

# Per-thread state for the inline driving wire.  Only Thread 2 (the
# asyncio / dispatch thread) reads or writes these attributes:
#
#   inline_active  (bool)  — True while the dispatch loop is between
#                            enter_inline() and leave_inline().
#   deferred_queue (deque)  — callbacks captured during inline mode,
#                            replayed when inline mode ends.
_dispatch_local = threading.local()

# Maximum callbacks drained per ``flush_deferred_callbacks`` invocation.
# Caps work between inline steps to prevent starving the event loop.
# Matches the 3-thread model's crossbeam channel capacity of 256.
_DRAIN_BUDGET: int = 64


# ---------------------------------------------------------------------------
# Wire installation
# ---------------------------------------------------------------------------


def install_call_soon_wire(loop: asyncio.AbstractEventLoop) -> OriginalCallSoon:
    """Replace ``loop.call_soon`` with a capturing variant.

    Returns the original ``call_soon`` so callers can replay captured
    callbacks through the real scheduling path.

    The replacement checks a thread-local flag
    (``_dispatch_local.inline_active``):

    - **True** (inline driving active): callbacks are appended to
      ``_dispatch_local.deferred_queue`` instead of landing in
      ``loop._ready``.
    - **False** (normal mode): delegates directly to the original
      ``call_soon`` — zero overhead on the normal asyncio path.
    """
    original: OriginalCallSoon = loop.call_soon

    def _intercepting_call_soon(
        callback: Callable[..., object],
        *args: object,
        context: contextvars.Context | None = None,
    ) -> None:
        if getattr(_dispatch_local, "inline_active", False):
            _dispatch_local.deferred_queue.append((callback, args, context))
        else:
            if context is not None:
                original(callback, *args, context=context)
            else:
                original(callback, *args)

    loop.call_soon = _intercepting_call_soon  # type: ignore[assignment]
    return original


# ---------------------------------------------------------------------------
# Inline mode lifecycle
# ---------------------------------------------------------------------------)


def enter_inline() -> None:
    """Enter inline driving mode — start capturing ``call_soon`` callbacks.

    Must be called before the first ``_enter_task`` / ``coro.send`` in
    each inline batch.  Paired with ``leave_inline`` which replays the
    captured callbacks through the original ``call_soon``.
    """
    _dispatch_local.inline_active = True
    _dispatch_local.deferred_queue = collections.deque()


def leave_inline(original: OriginalCallSoon) -> None:
    """Leave inline mode and replay captured callbacks.

    All callbacks captured during inline mode are flushed through the
    *original* ``loop.call_soon`` so they land in ``loop._ready`` and get
    processed during the next ``_run_once`` iteration — after our
    ``_leave_task`` has cleared ``current_task()``.
    """
    _dispatch_local.inline_active = False
    queue: collections.deque[DeferredCallback] | None = getattr(
        _dispatch_local, "deferred_queue", None
    )
    if queue:
        for callback, args, context in queue:
            if context is not None:
                original(callback, *args, context=context)
            else:
                original(callback, *args)
    _dispatch_local.deferred_queue = None


def flush_deferred_callbacks() -> None:
    """Drain up to ``_DRAIN_BUDGET`` captured callbacks mid-drive.

    Called between ``_leave_task`` and the next ``_enter_task`` when the
    coroutine suspends (yielded a Future).  Processes callbacks that
    accumulated during the inline step — e.g. done-callbacks from
    resolved Futures — while ``current_task() is None`` (safe window).

    Each callback is executed in its captured ``contextvars.Context``
    when one was provided to the original ``call_soon`` call, preserving
    the context semantics that asyncio guarantees.

    Leaves excess callbacks in the queue for the next call or for
    ``leave_inline`` to flush.
    """
    queue: collections.deque[DeferredCallback] | None = getattr(
        _dispatch_local, "deferred_queue", None
    )
    if not queue:
        return
    remaining = _DRAIN_BUDGET
    while queue and remaining > 0:
        callback, args, context = queue.popleft()
        if context is not None:
            context.run(callback, *args)
        else:
            callback(*args)
        remaining -= 1
