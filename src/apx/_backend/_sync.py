from __future__ import annotations

import asyncio
from typing import Any, Callable

from apx._core import Event, Lock, Semaphore


class ApxLock:
    """anyio-compatible Lock wrapping a Rust Lock primitive."""

    def __init__(
        self, rust_lock: Lock, fast_acquire: bool, checkpoint: Callable[..., Any]
    ) -> None:
        self._lock = rust_lock
        self._fast_acquire = fast_acquire
        self._checkpoint = checkpoint
        self._owner_task: asyncio.Task[object] | None = None

    async def acquire(self) -> None:
        if not self._fast_acquire:
            await self._checkpoint()
        self._owner_task = asyncio.current_task()
        await self._lock.acquire()

    def acquire_nowait(self) -> None:
        if self._lock.locked():
            from anyio import WouldBlock

            raise WouldBlock()
        self._owner_task = asyncio.current_task()

    def release(self) -> None:
        self._owner_task = None

    @property
    def locked(self) -> bool:
        return self._lock.locked()

    def statistics(self) -> Any:
        from anyio import LockStatistics, TaskInfo

        owner = None
        if self._owner_task is not None:
            t = self._owner_task
            owner = TaskInfo(
                id=id(t),
                parent_id=None,
                name=getattr(t, "get_name", lambda: str(t))(),
                coro=getattr(t, "get_coro", lambda: None)(),
            )
        return LockStatistics(
            locked=self._lock.locked(), owner=owner, tasks_waiting=0
        )

    async def __aenter__(self) -> ApxLock:
        await self.acquire()
        return self

    async def __aexit__(self, *exc: object) -> None:
        self.release()


class ApxSemaphore:
    """anyio-compatible Semaphore wrapping a Rust Semaphore primitive."""

    def __init__(
        self,
        rust_sem: Semaphore,
        max_value: int | None = None,
        fast_acquire: bool = False,
        checkpoint: Callable[..., Any] | None = None,
    ) -> None:
        self._sem = rust_sem
        self._max_value = max_value
        self._fast_acquire = fast_acquire
        self._checkpoint = checkpoint

    async def acquire(self) -> None:
        if not self._fast_acquire:
            await self._checkpoint()  # type: ignore[misc]
        await self._sem.acquire()

    def acquire_nowait(self) -> None:
        if self._sem.available_permits() == 0:
            from anyio import WouldBlock

            raise WouldBlock()

    def release(self) -> None:
        pass  # Rust semaphore permit is released when guard is dropped

    @property
    def value(self) -> int:
        return self._sem.available_permits()

    @property
    def max_value(self) -> int | None:
        return self._max_value

    def statistics(self) -> Any:
        from anyio import SemaphoreStatistics

        return SemaphoreStatistics(tasks_waiting=0)

    async def __aenter__(self) -> ApxSemaphore:
        await self.acquire()
        return self

    async def __aexit__(self, *exc: object) -> None:
        self.release()


class ApxCapacityLimiter:
    """anyio-compatible CapacityLimiter with borrower tracking."""

    def __init__(
        self,
        total_tokens: float,
        create_event: Callable[[], Event],
        checkpoint: Callable[..., Any],
    ) -> None:
        self._total_tokens = total_tokens
        self._borrowed_tokens = 0
        self._borrowers: set[object] = set()
        self._wait_queue: dict[object, Event] = {}
        self._create_event = create_event
        self._checkpoint = checkpoint

    async def acquire(self) -> None:
        await self.acquire_on_behalf_of(self._current_borrower())

    async def acquire_on_behalf_of(self, borrower: object) -> None:
        if borrower in self._borrowers:
            raise RuntimeError(
                "this borrower is already holding one of this CapacityLimiter's tokens"
            )
        if self._borrowed_tokens < self._total_tokens and not self._wait_queue:
            self._borrowed_tokens += 1
            self._borrowers.add(borrower)
            await self._checkpoint()
            return
        event = self._create_event()
        self._wait_queue[borrower] = event
        try:
            await event.wait()
        except BaseException:
            self._wait_queue.pop(borrower, None)
            raise

    def acquire_nowait(self) -> None:
        self.acquire_on_behalf_of_nowait(self._current_borrower())

    def acquire_on_behalf_of_nowait(self, borrower: object) -> None:
        if borrower in self._borrowers:
            raise RuntimeError(
                "this borrower is already holding one of this CapacityLimiter's tokens"
            )
        if self._borrowed_tokens >= self._total_tokens or self._wait_queue:
            from anyio import WouldBlock

            raise WouldBlock()
        self._borrowed_tokens += 1
        self._borrowers.add(borrower)

    def release(self) -> None:
        self.release_on_behalf_of(self._current_borrower())

    def release_on_behalf_of(self, borrower: object) -> None:
        if borrower not in self._borrowers:
            raise RuntimeError(
                "this borrower isn't holding any of this CapacityLimiter's tokens"
            )
        self._borrowers.discard(borrower)
        self._borrowed_tokens -= 1
        self._wake_next()

    def _wake_next(self) -> None:
        while self._wait_queue and self._borrowed_tokens < self._total_tokens:
            borrower, event = next(iter(self._wait_queue.items()))
            del self._wait_queue[borrower]
            self._borrowed_tokens += 1
            self._borrowers.add(borrower)
            event.set()

    def _current_borrower(self) -> asyncio.Task[object]:
        task = asyncio.current_task()
        if task is None:
            raise RuntimeError("no current task")
        return task

    @property
    def total_tokens(self) -> float:
        return self._total_tokens

    @total_tokens.setter
    def total_tokens(self, value: float) -> None:
        if not isinstance(value, (int, float)):
            raise TypeError("total_tokens must be a number")
        if value < 1:
            raise ValueError("total_tokens must be >= 1")
        old = self._total_tokens
        self._total_tokens = value
        if value > old:
            self._wake_next()

    @property
    def borrowed_tokens(self) -> int:
        return self._borrowed_tokens

    @property
    def available_tokens(self) -> float:
        return self._total_tokens - self._borrowed_tokens

    def statistics(self) -> Any:
        from anyio import CapacityLimiterStatistics

        return CapacityLimiterStatistics(
            borrowed_tokens=self._borrowed_tokens,
            total_tokens=self._total_tokens,
            borrowers=tuple(self._borrowers),
            tasks_waiting=len(self._wait_queue),
        )

    async def __aenter__(self) -> ApxCapacityLimiter:
        await self.acquire()
        return self

    async def __aexit__(self, *exc: object) -> None:
        self.release()
