from __future__ import annotations

import asyncio
from typing import Any, Callable

from anyio.abc import AsyncBackend

from apx._backend._cancel_scope import ApxCancelScope, _task_states
from apx._backend._memory_stream import (
    MemoryObjectReceiveStream,
    MemoryObjectSendStream,
)
from apx._backend._sync import ApxCapacityLimiter, ApxLock, ApxSemaphore
from apx._backend._task_group import ApxTaskGroup
from apx._core import ApxSchedulerCore


class ApxBackend(AsyncBackend):
    def __init__(
        self,
        core: ApxSchedulerCore,
        cancel_scope_cls: type[ApxCancelScope],
        task_group_cls: type[ApxTaskGroup],
        create_stream_pair: Callable[..., tuple[MemoryObjectSendStream, MemoryObjectReceiveStream]],
        task_states: dict[int, Any],
    ) -> None:
        self._core = core
        self._cancel_scope_cls = cancel_scope_cls
        self._task_group_cls = task_group_cls
        self._create_stream_pair = create_stream_pair
        self._task_states = task_states

    async def sleep(self, delay: float) -> None:
        await self._core.sleep(delay)

    def create_event(self) -> Any:
        return self._core.create_event()

    def create_cancel_scope(
        self, *, deadline: float = float("inf"), shield: bool = False
    ) -> Any:
        state = self._core.create_cancel_scope_state(deadline, shield)
        return self._cancel_scope_cls(state)

    def create_task_group(self) -> Any:
        core = self._core.create_task_group_core()
        state = self._core.create_cancel_scope_state()
        scope = self._cancel_scope_cls(state)
        return self._task_group_cls(core, scope)

    async def run_sync_in_worker_thread(  # type: ignore[override]
        self, func: Any, *, abandon_on_cancel: bool = False, limiter: Any = None
    ) -> Any:
        return await self._core.run_sync_in_worker_thread(func, abandon_on_cancel)

    def create_memory_object_stream(
        self, max_buffer_size: float = 0, item_type: Any = None
    ) -> tuple[MemoryObjectSendStream, MemoryObjectReceiveStream]:
        return self._create_stream_pair(max_buffer_size, self._core.create_event)

    async def checkpoint(self) -> None:
        await self._core.checkpoint()

    def current_time(self) -> float:
        return self._core.current_time()

    def current_token(self) -> ApxSchedulerCore:
        return self._core.current_token()

    @property
    def cancelled_exception_class(self) -> type[BaseException]:
        return self._core.cancelled_exception_class()

    # -- Cancel scope completion --

    def check_cancelled(self) -> None:
        task = asyncio.current_task()
        if task is None:
            return
        ts = self._task_states.get(id(task))
        if ts is None or ts.cancel_scope is None:
            return
        scope = ts.cancel_scope
        if not scope._state.shield and scope._state.is_effectively_cancelled():
            raise asyncio.CancelledError()

    async def checkpoint_if_cancelled(self) -> None:
        self.check_cancelled()

    async def cancel_shielded_checkpoint(self) -> None:
        await self._core.checkpoint()

    def current_effective_deadline(self) -> float:
        task = asyncio.current_task()
        if task is None:
            return float("inf")
        ts = self._task_states.get(id(task))
        if ts is None:
            return float("inf")
        scope = ts.cancel_scope
        deadline = float("inf")
        while scope is not None:
            if scope._state.shield:
                break
            scope_deadline = scope._state.deadline
            if scope_deadline < deadline:
                deadline = scope_deadline
            scope = scope._parent_scope
        return deadline

    # -- Sync primitives --

    def create_lock(self, *, fast_acquire: bool = False) -> Any:
        return ApxLock(
            self._core.create_lock_primitive(), fast_acquire, self._core.checkpoint
        )

    def create_semaphore(
        self,
        initial_value: int,
        *,
        max_value: int | None = None,
        fast_acquire: bool = False,
    ) -> Any:
        return ApxSemaphore(
            self._core.create_semaphore_primitive(initial_value),
            max_value=max_value,
            fast_acquire=fast_acquire,
            checkpoint=self._core.checkpoint,
        )

    def create_capacity_limiter(self, total_tokens: float) -> Any:
        return ApxCapacityLimiter(
            total_tokens, self.create_event, self._core.checkpoint
        )

    # -- Task introspection --

    def current_default_thread_limiter(self) -> Any:
        if not hasattr(self, "_default_thread_limiter"):
            self._default_thread_limiter = ApxCapacityLimiter(
                40, self.create_event, self._core.checkpoint
            )
        return self._default_thread_limiter

    def get_current_task(self) -> Any:
        from anyio import TaskInfo

        task = asyncio.current_task()
        if task is None:
            raise RuntimeError("no current task")
        return TaskInfo(
            id=id(task),
            parent_id=None,
            name=getattr(task, "get_name", lambda: str(task))(),
            coro=getattr(task, "get_coro", lambda: None)(),
        )

    def get_running_tasks(self) -> list[Any]:
        from anyio import TaskInfo

        return [
            TaskInfo(
                id=id(t),
                parent_id=None,
                name=t.get_name() if hasattr(t, "get_name") else str(t),
                coro=getattr(t, "get_coro", lambda: None)(),
            )
            for t in asyncio.all_tasks()
        ]

    async def wait_all_tasks_blocked(self) -> None:
        await self._core.checkpoint()

    # -- Thread bridge --

    def run_sync_from_thread(self, func: Any, args: tuple[Any, ...], token: Any) -> Any:
        return func(*args)

    def run_async_from_thread(self, func: Any, args: tuple[Any, ...], token: Any) -> Any:
        loop = asyncio.get_event_loop()
        future = asyncio.run_coroutine_threadsafe(func(*args), loop)
        return future.result()

    def create_blocking_portal(self) -> Any:
        from anyio.from_thread import BlockingPortal

        return BlockingPortal()

    # -- Process & signal --

    async def open_process(
        self,
        command: Any,
        *,
        stdin: Any = None,
        stdout: Any = None,
        stderr: Any = None,
        **kwargs: Any,
    ) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.open_process(
            command, stdin=stdin, stdout=stdout, stderr=stderr, **kwargs
        )

    def open_signal_receiver(self, *signals: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.open_signal_receiver(*signals)

    def setup_process_pool_exit_at_shutdown(self, workers: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.setup_process_pool_exit_at_shutdown(workers)

    # -- Entry point & test runner --

    def run(self, func: Any, args: tuple[Any, ...], kwargs: dict[str, Any], options: Any) -> Any:
        return asyncio.run(func(*args, **kwargs))

    def create_test_runner(self, options: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.create_test_runner(options)

    # -- Networking delegation (cold path) --

    async def connect_tcp(
        self, host: Any, port: Any, local_address: Any = None
    ) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.connect_tcp(host, port, local_address)

    async def connect_unix(self, path: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.connect_unix(path)

    def create_tcp_listener(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.create_tcp_listener(sock)

    def create_unix_listener(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.create_unix_listener(sock)

    async def create_udp_socket(
        self, family: Any, local_address: Any, remote_address: Any, reuse_port: Any
    ) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.create_udp_socket(
            family, local_address, remote_address, reuse_port
        )

    async def create_unix_datagram_socket(
        self, raw_socket: Any, remote_path: Any
    ) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.create_unix_datagram_socket(
            raw_socket, remote_path
        )

    async def getaddrinfo(
        self,
        host: Any,
        port: Any,
        *,
        family: int = 0,
        type: int = 0,
        proto: int = 0,
        flags: int = 0,
    ) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.getaddrinfo(
            host, port, family=family, type=type, proto=proto, flags=flags
        )

    async def getnameinfo(self, sockaddr: Any, flags: int = 0) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.getnameinfo(sockaddr, flags)

    async def wait_readable(self, obj: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wait_readable(obj)

    async def wait_writable(self, obj: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wait_writable(obj)

    def notify_closing(self, obj: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return AsyncIOBackend.notify_closing(obj)

    async def wrap_stream_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_stream_socket(sock)

    async def wrap_unix_stream_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_unix_stream_socket(sock)

    async def wrap_listener_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_listener_socket(sock)

    async def wrap_udp_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_udp_socket(sock)

    async def wrap_connected_udp_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_connected_udp_socket(sock)

    async def wrap_unix_datagram_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_unix_datagram_socket(sock)

    async def wrap_connected_unix_datagram_socket(self, sock: Any) -> Any:
        from anyio._backends._asyncio import AsyncIOBackend

        return await AsyncIOBackend.wrap_connected_unix_datagram_socket(sock)
