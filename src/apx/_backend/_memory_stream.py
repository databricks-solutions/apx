from __future__ import annotations

from collections import OrderedDict, deque
from typing import Any, Callable

from anyio import (
    BrokenResourceError,
    ClosedResourceError,
    EndOfStream,
    WouldBlock,
)
from anyio.abc import ObjectReceiveStream, ObjectSendStream

from apx._core import Event


class MemoryObjectStreamState:
    """Shared state between send and receive ends."""

    __slots__ = (
        "buffer",
        "max_buffer_size",
        "open_send_channels",
        "open_receive_channels",
        "waiting_receivers",
        "waiting_senders",
    )

    def __init__(self, max_buffer_size: float) -> None:
        self.buffer: deque[Any] = deque()
        self.max_buffer_size = max_buffer_size
        self.open_send_channels = 0
        self.open_receive_channels = 0
        self.waiting_receivers: OrderedDict[Event, None] = OrderedDict()
        self.waiting_senders: OrderedDict[Event, Any] = OrderedDict()


class MemoryObjectReceiveStream(ObjectReceiveStream[Any]):
    """Receive end of a memory object stream."""

    def __init__(
        self, state: MemoryObjectStreamState, create_event: Callable[[], Event]
    ) -> None:
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_receive_channels += 1

    async def receive(self) -> Any:
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if state.waiting_senders:
            event, item = next(iter(state.waiting_senders.items()))
            del state.waiting_senders[event]
            event.set()
            return item

        if state.buffer:
            return state.buffer.popleft()

        if not state.open_send_channels:
            raise EndOfStream

        event = self._create_event()
        state.waiting_receivers[event] = None
        try:
            await event.wait()
        except BaseException:
            state.waiting_receivers.pop(event, None)
            raise

        if state.buffer:
            return state.buffer.popleft()

        if state.waiting_senders:
            event_s, item = next(iter(state.waiting_senders.items()))
            del state.waiting_senders[event_s]
            event_s.set()
            return item

        raise EndOfStream

    def receive_nowait(self) -> Any:
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if state.waiting_senders:
            event, item = next(iter(state.waiting_senders.items()))
            del state.waiting_senders[event]
            event.set()
            return item

        if state.buffer:
            return state.buffer.popleft()

        raise WouldBlock

    def clone(self) -> MemoryObjectReceiveStream:
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectReceiveStream(self._state, self._create_event)

    async def aclose(self) -> None:
        self.close()

    def close(self) -> None:
        if not self._closed:
            self._closed = True
            self._state.open_receive_channels -= 1
            if self._state.open_receive_channels == 0:
                for event in list(self._state.waiting_senders):
                    event.set()
                self._state.waiting_senders.clear()

    def __del__(self) -> None:
        if not self._closed:
            self.close()

    @property
    def _open(self) -> bool:
        return not self._closed

    def statistics(self) -> _StreamStatistics:
        state = self._state
        return _StreamStatistics(
            current_buffer_used=len(state.buffer),
            max_buffer_size=state.max_buffer_size,
            open_send_streams=state.open_send_channels,
            open_receive_streams=state.open_receive_channels,
            tasks_waiting_send=len(state.waiting_senders),
            tasks_waiting_receive=len(state.waiting_receivers),
        )


class MemoryObjectSendStream(ObjectSendStream[Any]):
    """Send end of a memory object stream."""

    def __init__(
        self, state: MemoryObjectStreamState, create_event: Callable[[], Event]
    ) -> None:
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_send_channels += 1

    async def send(self, item: Any) -> None:
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if not state.open_receive_channels:
            raise BrokenResourceError

        if state.waiting_receivers:
            event = next(iter(state.waiting_receivers))
            del state.waiting_receivers[event]
            state.buffer.append(item)
            event.set()
            return

        if len(state.buffer) < state.max_buffer_size:
            state.buffer.append(item)
            return

        event = self._create_event()
        state.waiting_senders[event] = item
        try:
            await event.wait()
        except BaseException:
            state.waiting_senders.pop(event, None)
            raise

        if not state.open_receive_channels:
            raise BrokenResourceError

    def send_nowait(self, item: Any) -> None:
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if not state.open_receive_channels:
            raise BrokenResourceError

        if state.waiting_receivers:
            event = next(iter(state.waiting_receivers))
            del state.waiting_receivers[event]
            state.buffer.append(item)
            event.set()
            return

        if len(state.buffer) < state.max_buffer_size:
            state.buffer.append(item)
            return

        raise WouldBlock

    def clone(self) -> MemoryObjectSendStream:
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectSendStream(self._state, self._create_event)

    async def aclose(self) -> None:
        self.close()

    def close(self) -> None:
        if not self._closed:
            self._closed = True
            self._state.open_send_channels -= 1
            if self._state.open_send_channels == 0:
                for event in list(self._state.waiting_receivers):
                    event.set()
                self._state.waiting_receivers.clear()

    def __del__(self) -> None:
        if not self._closed:
            self.close()

    @property
    def _open(self) -> bool:
        return not self._closed

    def statistics(self) -> _StreamStatistics:
        state = self._state
        return _StreamStatistics(
            current_buffer_used=len(state.buffer),
            max_buffer_size=state.max_buffer_size,
            open_send_streams=state.open_send_channels,
            open_receive_streams=state.open_receive_channels,
            tasks_waiting_send=len(state.waiting_senders),
            tasks_waiting_receive=len(state.waiting_receivers),
        )


class _StreamStatistics:
    __slots__ = (
        "current_buffer_used",
        "max_buffer_size",
        "open_send_streams",
        "open_receive_streams",
        "tasks_waiting_send",
        "tasks_waiting_receive",
    )

    def __init__(self, **kwargs: Any) -> None:
        for k, v in kwargs.items():
            setattr(self, k, v)


def create_memory_object_stream_pair(
    max_buffer_size: float, create_event: Callable[[], Event]
) -> tuple[MemoryObjectSendStream, MemoryObjectReceiveStream]:
    state = MemoryObjectStreamState(max_buffer_size)
    send = MemoryObjectSendStream(state, create_event)
    receive = MemoryObjectReceiveStream(state, create_event)
    return send, receive
