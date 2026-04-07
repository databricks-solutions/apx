"""WebSocket ASGI bridge using wsproto for frame encoding/decoding.

Bridges an asyncio transport to the ASGI WebSocket protocol. The HTTP
upgrade handshake (101 Switching Protocols) is handled externally by
the Rust protocol layer; this module handles only the post-handshake
WebSocket frame lifecycle.

Uses wsproto as a sans-I/O state machine — we own the transport, wsproto
owns the frame parsing.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import logging
from collections.abc import Callable, Coroutine
from typing import Any

from wsproto import ConnectionType
from wsproto.connection import Connection
from wsproto.events import (
    BytesMessage,
    CloseConnection,
    Ping,
    TextMessage,
)

logger = logging.getLogger("apx.websocket")

# RFC 6455 §4.2.2: magic GUID for Sec-WebSocket-Accept computation.
_WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def compute_accept_key(sec_websocket_key: str) -> str:
    """Compute ``Sec-WebSocket-Accept`` from the client's key (RFC 6455 §4.2.2)."""
    digest = hashlib.sha1(sec_websocket_key.encode() + _WS_GUID).digest()
    return base64.b64encode(digest).decode()


def build_upgrade_response(
    sec_websocket_key: str,
    subprotocol: str | None = None,
    extra_headers: list[tuple[bytes, bytes]] | None = None,
) -> bytes:
    """Build the HTTP 101 Switching Protocols response."""
    accept = compute_accept_key(sec_websocket_key)
    lines = [
        b"HTTP/1.1 101 Switching Protocols",
        b"Upgrade: websocket",
        b"Connection: Upgrade",
        f"Sec-WebSocket-Accept: {accept}".encode(),
    ]
    if subprotocol:
        lines.append(f"Sec-WebSocket-Protocol: {subprotocol}".encode())
    if extra_headers:
        for name, value in extra_headers:
            lines.append(name + b": " + value)
    lines.append(b"")
    lines.append(b"")
    return b"\r\n".join(lines)


class WebSocketBridge:
    """Bridges an asyncio transport to the ASGI WebSocket protocol.

    The Rust protocol creates this after detecting an HTTP upgrade
    request. Subsequent ``data_received`` calls on the protocol are
    forwarded to ``feed_data``, which parses WebSocket frames via
    wsproto and enqueues ASGI receive events.

    The ASGI app interacts with this bridge through the ``receive``
    and ``send`` callables passed to ``app(scope, receive, send)``.
    """

    __slots__ = (
        "_transport",
        "_app",
        "_scope",
        "_ws",
        "_receive_queue",
        "_closed",
        "_task",
    )

    def __init__(
        self,
        transport: Any,
        scope: dict[str, Any],
        app: Callable[..., Coroutine[Any, Any, None]],
    ) -> None:
        self._transport = transport
        self._scope = scope
        self._app = app
        # wsproto server connection starts in OPEN state — handshake
        # was handled by the Rust protocol (101 already written).
        self._ws = Connection(ConnectionType.SERVER)
        self._receive_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        self._closed = False
        self._task: asyncio.Task[None] | None = None

    def start(self) -> None:
        """Start the ASGI WebSocket lifecycle as an asyncio task."""
        # Queue the initial connect event per ASGI spec.
        self._receive_queue.put_nowait({"type": "websocket.connect"})
        self._task = asyncio.get_running_loop().create_task(self._run())

    async def _run(self) -> None:
        """Run the ASGI app with WebSocket receive/send callables."""
        try:
            await self._app(self._scope, self._receive, self._send)
        except Exception:
            logger.exception("WebSocket app error")
        finally:
            if not self._closed:
                self._close_transport()

    async def _receive(self) -> dict[str, Any]:
        """ASGI receive callable — blocks until a WS event is available."""
        return await self._receive_queue.get()

    async def _send(self, message: dict[str, Any]) -> None:
        """ASGI send callable — processes WebSocket events from the app."""
        msg_type = message["type"]
        if msg_type == "websocket.accept":
            self._handle_accept(message)
        elif msg_type == "websocket.send":
            self._handle_send(message)
        elif msg_type == "websocket.close":
            self._handle_close(message)

    def feed_data(self, data: bytes) -> None:
        """Feed raw bytes from the transport into the wsproto parser.

        Called by ``RustProtocol.data_received`` after the WebSocket
        upgrade is complete.
        """
        self._ws.receive_data(data)
        for event in self._ws.events():
            if isinstance(event, TextMessage):
                if event.message_finished:
                    self._receive_queue.put_nowait(
                        {"type": "websocket.receive", "text": event.data}
                    )
            elif isinstance(event, BytesMessage):
                if event.message_finished:
                    self._receive_queue.put_nowait(
                        {"type": "websocket.receive", "bytes": event.data}
                    )
            elif isinstance(event, CloseConnection):
                self._receive_queue.put_nowait(
                    {
                        "type": "websocket.disconnect",
                        "code": event.code or 1005,
                    }
                )
                # Send close acknowledgment
                data_out = self._ws.send(event.response())
                self._transport.write(data_out)
                self._closed = True
            elif isinstance(event, Ping):
                # Auto-respond to pings
                data_out = self._ws.send(event.response())
                self._transport.write(data_out)

    def connection_lost(self) -> None:
        """Called when the transport connection is lost."""
        if not self._closed:
            self._closed = True
            self._receive_queue.put_nowait(
                {"type": "websocket.disconnect", "code": 1006}
            )

    def _handle_accept(self, message: dict[str, Any]) -> None:
        """Process websocket.accept — the 101 was already sent by Rust.

        Extract subprotocol and extra headers if the app provided them,
        but in practice the 101 is already written. This is a no-op
        for the transport but required by the ASGI lifecycle.
        """
        # The 101 response was already written by Rust before this
        # bridge was created. Nothing to do here.

    def _handle_send(self, message: dict[str, Any]) -> None:
        """Process websocket.send — encode and write a WS frame."""
        if self._closed:
            return
        text = message.get("text")
        data_bytes = message.get("bytes")
        if text is not None:
            out = self._ws.send(TextMessage(data=text))
        elif data_bytes is not None:
            out = self._ws.send(BytesMessage(data=data_bytes))
        else:
            return
        self._transport.write(out)

    def _handle_close(self, message: dict[str, Any]) -> None:
        """Process websocket.close — send close frame and close transport."""
        if self._closed:
            return
        code = message.get("code", 1000)
        reason = message.get("reason", "")
        out = self._ws.send(CloseConnection(code=code, reason=reason))
        self._transport.write(out)
        self._closed = True
        self._close_transport()

    def _close_transport(self) -> None:
        """Close the underlying transport."""
        try:
            self._transport.close()
        except Exception:
            pass
