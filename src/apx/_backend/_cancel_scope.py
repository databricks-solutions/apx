from __future__ import annotations

import asyncio

from apx._core import CancelScopeState


class _TaskState:
    """Per-task cancellation tracking."""

    __slots__ = ("cancel_scope",)

    def __init__(self) -> None:
        self.cancel_scope: ApxCancelScope | None = None


_task_states: dict[int, _TaskState] = {}


class ApxCancelScope:
    """CancelScope compatible with anyio's interface.

    Uses CancelScopeState (Rust) for fast deadline/cancel checks.
    Manages scope tree via _task_states dict.
    """

    def __init__(self, state: CancelScopeState) -> None:
        self._state = state
        self._parent_scope: ApxCancelScope | None = None
        self._host_task: asyncio.Task[object] | None = None
        self._active = False

    def __enter__(self) -> ApxCancelScope:
        task = asyncio.current_task()
        if task is None:
            raise RuntimeError("CancelScope requires a running task")

        self._host_task = task
        task_id = id(task)
        if task_id not in _task_states:
            _task_states[task_id] = _TaskState()

        ts = _task_states[task_id]
        self._parent_scope = ts.cancel_scope
        ts.cancel_scope = self
        self._active = True
        self._state.host_task_state = ts
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: object | None,
    ) -> bool:
        self._active = False
        task_id = id(self._host_task)
        if task_id in _task_states:
            _task_states[task_id].cancel_scope = self._parent_scope
        self._state.host_task_state = None

        if exc_type is not None and self._state.is_effectively_cancelled():
            if issubclass(exc_type, BaseException) and _is_cancellation(exc_val):
                self._state.cancelled_caught = True
                return True

        return False

    def cancel(self) -> None:
        self._state.cancel()

    @property
    def deadline(self) -> float:
        return self._state.deadline

    @deadline.setter
    def deadline(self, value: float) -> None:
        self._state.deadline = value

    @property
    def shield(self) -> bool:
        return self._state.shield

    @shield.setter
    def shield(self, value: bool) -> None:
        self._state.shield = value

    @property
    def cancel_called(self) -> bool:
        return self._state.cancel_called

    @property
    def cancelled_caught(self) -> bool:
        return self._state.cancelled_caught

    @property
    def cancel_scope(self) -> ApxCancelScope:
        """Return self - anyio expects cancel_scope attribute on CancelScope."""
        return self


def _is_cancellation(exc: BaseException | None) -> bool:
    """Check if an exception is an asyncio.CancelledError."""
    return isinstance(exc, asyncio.CancelledError)


def _check_cancel_scope_for_task(task_id: int) -> bool:
    """Check if the current task's innermost scope is effectively cancelled.

    Called from the Rust driver at yield points (checkpoints).
    Returns True if a CancelledError should be thrown.
    """
    ts = _task_states.get(task_id)
    if ts is None or ts.cancel_scope is None:
        return False
    scope = ts.cancel_scope
    if scope._state.shield:
        return False
    return scope._state.is_effectively_cancelled()
