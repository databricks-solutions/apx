//! MemoryObjectStream — pure Python implementation using Rust Event primitives.
//!
//! Provides `MemoryObjectSendStream` and `MemoryObjectReceiveStream` backed
//! by our Rust `Event` for signaling and Python `deque` for buffering.
//!
//! This is a port of anyio's reference implementation with our primitives
//! substituted for the signaling mechanism.

/// Python source for the memory object stream classes.
///
/// Implements anyio's `MemoryObjectSendStream` and `MemoryObjectReceiveStream`
/// interfaces using `Event` (Rust-backed) for signaling and Python `deque`
/// for buffering.
#[expect(dead_code, reason = "consumed in Phase 5 via anyio_backend rewrite")]
pub const MEMORY_STREAM_GLUE: &str = r#"
from collections import deque, OrderedDict
from anyio import (
    ClosedResourceError,
    BrokenResourceError,
    EndOfStream,
    WouldBlock,
)
from anyio.abc import ObjectSendStream, ObjectReceiveStream


class MemoryObjectStreamState:
    """Shared state between send and receive ends."""

    __slots__ = (
        'buffer', 'max_buffer_size',
        'open_send_channels', 'open_receive_channels',
        'waiting_receivers', 'waiting_senders',
    )

    def __init__(self, max_buffer_size):
        self.buffer = deque()
        self.max_buffer_size = max_buffer_size
        self.open_send_channels = 0
        self.open_receive_channels = 0
        self.waiting_receivers = OrderedDict()  # Event -> None
        self.waiting_senders = OrderedDict()    # Event -> item


class MemoryObjectReceiveStream(ObjectReceiveStream):
    """Receive end of a memory object stream."""

    def __init__(self, state, create_event):
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_receive_channels += 1

    async def receive(self):
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if state.waiting_senders:
            # A sender is waiting — take their item
            event, item = next(iter(state.waiting_senders.items()))
            del state.waiting_senders[event]
            event.set()
            return item

        if state.buffer:
            return state.buffer.popleft()

        if not state.open_send_channels:
            raise EndOfStream

        # Wait for an item
        event = self._create_event()
        state.waiting_receivers[event] = None
        try:
            await event.wait()
        except BaseException:
            state.waiting_receivers.pop(event, None)
            raise

        # After wake-up, check for items
        if state.buffer:
            return state.buffer.popleft()

        if state.waiting_senders:
            event_s, item = next(iter(state.waiting_senders.items()))
            del state.waiting_senders[event_s]
            event_s.set()
            return item

        raise EndOfStream

    def receive_nowait(self):
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

    def clone(self):
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectReceiveStream(self._state, self._create_event)

    async def aclose(self):
        self.close()

    def close(self):
        if not self._closed:
            self._closed = True
            self._state.open_receive_channels -= 1
            if self._state.open_receive_channels == 0:
                # Wake all waiting senders
                for event in list(self._state.waiting_senders):
                    event.set()
                self._state.waiting_senders.clear()

    def __del__(self):
        if not self._closed:
            self.close()

    @property
    def _open(self):
        return not self._closed

    def statistics(self):
        state = self._state
        return _StreamStatistics(
            current_buffer_used=len(state.buffer),
            max_buffer_size=state.max_buffer_size,
            open_send_streams=state.open_send_channels,
            open_receive_streams=state.open_receive_channels,
            tasks_waiting_send=len(state.waiting_senders),
            tasks_waiting_receive=len(state.waiting_receivers),
        )


class MemoryObjectSendStream(ObjectSendStream):
    """Send end of a memory object stream."""

    def __init__(self, state, create_event):
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_send_channels += 1

    async def send(self, item):
        if self._closed:
            raise ClosedResourceError

        state = self._state

        if not state.open_receive_channels:
            raise BrokenResourceError

        if state.waiting_receivers:
            # A receiver is waiting — put item in buffer and wake them
            event = next(iter(state.waiting_receivers))
            del state.waiting_receivers[event]
            state.buffer.append(item)
            event.set()
            return

        if len(state.buffer) < state.max_buffer_size:
            state.buffer.append(item)
            return

        # Buffer full — wait
        event = self._create_event()
        state.waiting_senders[event] = item
        try:
            await event.wait()
        except BaseException:
            state.waiting_senders.pop(event, None)
            raise

        if not state.open_receive_channels:
            raise BrokenResourceError

    def send_nowait(self, item):
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

    def clone(self):
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectSendStream(self._state, self._create_event)

    async def aclose(self):
        self.close()

    def close(self):
        if not self._closed:
            self._closed = True
            self._state.open_send_channels -= 1
            if self._state.open_send_channels == 0:
                # Wake all waiting receivers
                for event in list(self._state.waiting_receivers):
                    event.set()
                self._state.waiting_receivers.clear()

    def __del__(self):
        if not self._closed:
            self.close()

    @property
    def _open(self):
        return not self._closed

    def statistics(self):
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
        'current_buffer_used', 'max_buffer_size',
        'open_send_streams', 'open_receive_streams',
        'tasks_waiting_send', 'tasks_waiting_receive',
    )

    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


def create_memory_object_stream_pair(max_buffer_size, create_event):
    state = MemoryObjectStreamState(max_buffer_size)
    send = MemoryObjectSendStream(state, create_event)
    receive = MemoryObjectReceiveStream(state, create_event)
    return send, receive
"#;

/// Evaluate the memory stream Python glue and return the module dict.
pub fn eval_memory_stream_glue(py: pyo3::Python<'_>) -> pyo3::PyResult<pyo3::Py<pyo3::PyAny>> {
    let code = std::ffi::CString::new(
        r#"
from collections import deque, OrderedDict
from anyio import (
    ClosedResourceError,
    BrokenResourceError,
    EndOfStream,
    WouldBlock,
)
from anyio.abc import ObjectSendStream, ObjectReceiveStream


class MemoryObjectStreamState:
    __slots__ = (
        'buffer', 'max_buffer_size',
        'open_send_channels', 'open_receive_channels',
        'waiting_receivers', 'waiting_senders',
    )

    def __init__(self, max_buffer_size):
        self.buffer = deque()
        self.max_buffer_size = max_buffer_size
        self.open_send_channels = 0
        self.open_receive_channels = 0
        self.waiting_receivers = OrderedDict()
        self.waiting_senders = OrderedDict()


class MemoryObjectReceiveStream(ObjectReceiveStream):
    def __init__(self, state, create_event):
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_receive_channels += 1

    async def receive(self):
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

    def receive_nowait(self):
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

    def clone(self):
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectReceiveStream(self._state, self._create_event)

    async def aclose(self):
        self.close()

    def close(self):
        if not self._closed:
            self._closed = True
            self._state.open_receive_channels -= 1
            if self._state.open_receive_channels == 0:
                for event in list(self._state.waiting_senders):
                    event.set()
                self._state.waiting_senders.clear()

    def __del__(self):
        if not self._closed:
            self.close()

    @property
    def _open(self):
        return not self._closed

    def statistics(self):
        state = self._state
        return _StreamStatistics(
            current_buffer_used=len(state.buffer),
            max_buffer_size=state.max_buffer_size,
            open_send_streams=state.open_send_channels,
            open_receive_streams=state.open_receive_channels,
            tasks_waiting_send=len(state.waiting_senders),
            tasks_waiting_receive=len(state.waiting_receivers),
        )


class MemoryObjectSendStream(ObjectSendStream):
    def __init__(self, state, create_event):
        self._state = state
        self._create_event = create_event
        self._closed = False
        state.open_send_channels += 1

    async def send(self, item):
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

    def send_nowait(self, item):
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

    def clone(self):
        if self._closed:
            raise ClosedResourceError
        return MemoryObjectSendStream(self._state, self._create_event)

    async def aclose(self):
        self.close()

    def close(self):
        if not self._closed:
            self._closed = True
            self._state.open_send_channels -= 1
            if self._state.open_send_channels == 0:
                for event in list(self._state.waiting_receivers):
                    event.set()
                self._state.waiting_receivers.clear()

    def __del__(self):
        if not self._closed:
            self.close()

    @property
    def _open(self):
        return not self._closed

    def statistics(self):
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
        'current_buffer_used', 'max_buffer_size',
        'open_send_streams', 'open_receive_streams',
        'tasks_waiting_send', 'tasks_waiting_receive',
    )
    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


def create_memory_object_stream_pair(max_buffer_size, create_event):
    state = MemoryObjectStreamState(max_buffer_size)
    send = MemoryObjectSendStream(state, create_event)
    receive = MemoryObjectReceiveStream(state, create_event)
    return send, receive
"#,
    )?;

    let locals = pyo3::types::PyDict::new(py);
    py.run(&code, None, Some(&locals))?;
    Ok(locals.unbind().into_any())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use pyo3::types::PyDictMethods;

    #[test]
    fn memory_stream_glue_evaluates() {
        crate::with_py(|py| {
            // Skip if anyio is not installed (test-only environment).
            if py.import(c"anyio").is_err() {
                return;
            }
            let locals = eval_memory_stream_glue(py).unwrap();
            let locals = locals
                .into_bound(py)
                .cast_into::<pyo3::types::PyDict>()
                .unwrap();
            assert!(locals.get_item("MemoryObjectSendStream").unwrap().is_some());
            assert!(
                locals
                    .get_item("MemoryObjectReceiveStream")
                    .unwrap()
                    .is_some()
            );
            assert!(
                locals
                    .get_item("create_memory_object_stream_pair")
                    .unwrap()
                    .is_some()
            );
        });
    }
}
