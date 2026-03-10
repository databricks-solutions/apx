from __future__ import annotations

import asyncio
import sys
from typing import Any

from apx._backend._cancel_scope import ApxCancelScope
from apx._core import TaskGroupCore


class ApxTaskGroup:
    """TaskGroup compatible with anyio's interface."""

    def __init__(self, core: TaskGroupCore, cancel_scope: ApxCancelScope) -> None:
        self._core = core
        self.cancel_scope = cancel_scope
        self._host_task: asyncio.Task[object] | None = None
        self._tasks: list[asyncio.Task[object]] = []

    async def __aenter__(self) -> ApxTaskGroup:
        self._host_task = asyncio.current_task()
        self.cancel_scope.__enter__()
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: Any,
    ) -> bool:
        try:
            if self._core.has_pending():
                fut = self._core.get_completion_future()
                if fut is not None:
                    await fut
            else:
                self._core.resolve_if_empty()

            exceptions = self._core.get_exceptions()
            if exceptions:
                self.cancel_scope.cancel()
                if exc_val is not None:
                    exceptions.append(exc_val)
                if len(exceptions) == 1:
                    raise exceptions[0]
                raise BaseExceptionGroup("multiple child errors", exceptions)
        finally:
            self.cancel_scope.__exit__(*sys.exc_info())

        return False

    def start_soon(self, func: Any, *args: Any, name: str | None = None) -> None:
        coro = func(*args)
        self._core.child_spawned()

        loop = asyncio.get_running_loop()
        task = loop.create_task(coro)
        if name:
            task.set_name(name)
        self._tasks.append(task)

        def _on_done(t: asyncio.Task[object]) -> None:
            exc = None
            if not t.cancelled():
                exc = t.exception()
            if exc is not None:
                self._core.child_completed(exc)
            else:
                self._core.child_completed()

        task.add_done_callback(_on_done)

    async def start(self, func: Any, *args: Any, name: str | None = None) -> Any:
        task_status_future: asyncio.Future[Any] = asyncio.get_running_loop().create_future()

        class _TaskStatus:
            def __init__(self) -> None:
                self._started = False

            def started(self, value: Any = None) -> None:
                if self._started:
                    raise RuntimeError("started() called twice")
                self._started = True
                task_status_future.set_result(value)

        async def _wrapper() -> Any:
            return await func(*args, task_status=_TaskStatus())

        self.start_soon(_wrapper, name=name)
        return await task_status_future
