"""WebSocket integration tests for APX.

Tests run against the bench app's WebSocket endpoints inside the Docker
container managed by the session fixtures.
"""

from __future__ import annotations

import asyncio
import json
import os
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Any

import httpx
import pytest
from websockets.asyncio.client import ClientConnection, connect
from websockets.exceptions import ConnectionClosed, InvalidStatus
from websockets.frames import CloseCode

RECV_TIMEOUT: float = 5.0


@pytest.fixture(scope="session")
def ws_url(apx_container: str) -> str:
    """WebSocket base URL derived from the HTTP container URL."""
    return apx_container.replace("http://", "ws://")


@asynccontextmanager
async def ws_connect(
    base_url: str,
    path: str,
    **kwargs: Any,
) -> AsyncIterator[ClientConnection]:
    """Typed wrapper around websockets.connect."""
    async with connect(f"{base_url}{path}", **kwargs) as ws:
        yield ws


async def echo_roundtrip(ws: ClientConnection, message: str) -> str:
    """Send a text message and return the echo reply."""
    await ws.send(message)
    reply = await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
    assert isinstance(reply, str)
    return reply


async def binary_roundtrip(ws: ClientConnection, data: bytes) -> bytes:
    """Send a binary message and return the echo reply."""
    await ws.send(data)
    reply = await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
    assert isinstance(reply, bytes)
    return reply


# ---------------------------------------------------------------------------
# Echo
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketEcho:
    def test_single_message(self, ws_url: str) -> None:
        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                assert await echo_roundtrip(ws, "hello") == "hello"

        asyncio.run(_run())

    def test_multiple_messages(self, ws_url: str) -> None:
        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                for i in range(10):
                    assert await echo_roundtrip(ws, f"msg-{i}") == f"msg-{i}"

        asyncio.run(_run())

    def test_concurrent_connections(self, ws_url: str) -> None:
        async def worker(n: int) -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                for i in range(5):
                    msg = f"w{n}-{i}"
                    assert await echo_roundtrip(ws, msg) == msg

        async def _run() -> None:
            await asyncio.gather(*[worker(i) for i in range(10)])

        asyncio.run(_run())

    def test_client_disconnect(self, ws_url: str) -> None:
        """Server handles client disconnect gracefully."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                await echo_roundtrip(ws, "hello")

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# JSON echo
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketJSON:
    def test_json_echo(self, ws_url: str) -> None:
        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/json") as ws:
                payload = {"action": "ping", "n": 42}
                await ws.send(json.dumps(payload))
                raw = await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
                assert isinstance(raw, str)
                reply: dict[str, Any] = json.loads(raw)
                assert reply["action"] == "ping"
                assert reply["n"] == 42
                assert "server_ts" in reply

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Binary
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketBinary:
    @pytest.mark.parametrize(
        "payload",
        [
            pytest.param(b"\x00\x01\x02\xff", id="small"),
            pytest.param(b"", id="empty"),
            pytest.param(b"\x80" * 1024, id="1kb-repeated"),
        ],
    )
    def test_binary_echo(self, ws_url: str, payload: bytes) -> None:
        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/binary") as ws:
                assert await binary_roundtrip(ws, payload) == payload

        asyncio.run(_run())

    def test_large_binary_payload(self, ws_url: str) -> None:
        payload = os.urandom(1_000_000)

        async def _run() -> None:
            async with ws_connect(
                ws_url,
                "/api/ws/binary",
                max_size=2_000_000,
            ) as ws:
                assert await binary_roundtrip(ws, payload) == payload

        asyncio.run(_run())

    def test_mixed_text_and_binary(self, ws_url: str) -> None:
        """Alternate between text and binary on separate connections."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as text_ws:
                async with ws_connect(ws_url, "/api/ws/binary") as bin_ws:
                    for i in range(5):
                        assert await echo_roundtrip(text_ws, f"t-{i}") == f"t-{i}"
                        data = os.urandom(64)
                        assert await binary_roundtrip(bin_ws, data) == data

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Close codes
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketClose:
    @pytest.mark.parametrize(
        ("code", "reason"),
        [
            pytest.param(1000, "normal", id="normal"),
            pytest.param(1001, "going away", id="going-away"),
            pytest.param(4000, "app-defined", id="app-defined"),
        ],
    )
    def test_server_close(self, ws_url: str, code: int, reason: str) -> None:
        """Server closes with a specific code after receiving a JSON command."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/close-with-code") as ws:
                await ws.send(json.dumps({"code": code, "reason": reason}))
                with pytest.raises(ConnectionClosed) as exc_info:
                    await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
                assert exc_info.value.rcvd is not None
                assert exc_info.value.rcvd.code == code

        asyncio.run(_run())

    @pytest.mark.parametrize(
        "code",
        [
            pytest.param(CloseCode.NORMAL_CLOSURE, id="normal"),
            pytest.param(CloseCode.GOING_AWAY, id="going-away"),
        ],
    )
    def test_client_close(self, ws_url: str, code: CloseCode) -> None:
        """Client-initiated close with specific code; server stays healthy."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                await echo_roundtrip(ws, "before-close")
                await ws.close(code)

            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                assert await echo_roundtrip(ws, "still-alive") == "still-alive"

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Reject / routing errors
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketReject:
    def test_handler_rejects(self, ws_url: str) -> None:
        """App returns without calling accept; connection closes promptly.

        APX eagerly sends 101 before the ASGI app runs, so the WS
        handshake succeeds but the server-side immediately tears down.
        """

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/reject") as ws:
                with pytest.raises(ConnectionClosed):
                    await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)

        asyncio.run(_run())

    def test_nonexistent_route(self, ws_url: str) -> None:
        """Connecting to an unregistered path yields a refused upgrade."""

        async def _run() -> None:
            with pytest.raises((InvalidStatus, ConnectionClosed, OSError)):
                async with ws_connect(ws_url, "/api/ws/does-not-exist") as ws:
                    await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)

        asyncio.run(_run())

    def test_http_get_on_ws_route(self, client: httpx.Client) -> None:
        """Plain HTTP GET to a WebSocket route returns an error, not a crash."""
        r = client.get("/api/ws/echo")
        assert r.status_code >= 400


# ---------------------------------------------------------------------------
# Payload edge cases
# ---------------------------------------------------------------------------

_UNICODE_SAMPLES: str = (
    "\U0001f600"  # emoji (grinning face)
    "\u4e16\u754c"  # CJK (世界)
    "\u0645\u0631\u062d\u0628\u0627"  # Arabic (مرحبا)
    "\u00e9\u00e0\u00fc"  # Latin accented
)


@pytest.mark.integration
class TestWebSocketPayload:
    @pytest.mark.parametrize(
        "text",
        [
            pytest.param("", id="empty"),
            pytest.param("a", id="single-char"),
            pytest.param("hello world", id="ascii"),
            pytest.param(_UNICODE_SAMPLES, id="unicode"),
            pytest.param("x" * 1_000_000, id="1mb"),
        ],
    )
    def test_text_payload(self, ws_url: str, text: str) -> None:
        async def _run() -> None:
            async with ws_connect(
                ws_url, "/api/ws/echo", max_size=2_000_000
            ) as ws:
                assert await echo_roundtrip(ws, text) == text

        asyncio.run(_run())

    @pytest.mark.parametrize(
        "value",
        [
            pytest.param(None, id="null"),
            pytest.param([], id="empty-list"),
            pytest.param({}, id="empty-dict"),
            pytest.param(
                {"nested": {"a": [1, 2, None]}},
                id="nested",
            ),
            pytest.param("", id="empty-string"),
            pytest.param(0, id="zero"),
            pytest.param(True, id="bool"),
        ],
    )
    def test_json_special_values(
        self, ws_url: str, value: Any
    ) -> None:
        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/json") as ws:
                payload = {"action": "test", "v": value}
                await ws.send(json.dumps(payload))
                raw = await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
                assert isinstance(raw, str)
                reply: dict[str, Any] = json.loads(raw)
                assert reply["action"] == "test"
                assert reply["v"] == value
                assert "server_ts" in reply

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Concurrency
# ---------------------------------------------------------------------------

RAPID_FIRE_COUNT: int = 100
CONCURRENT_WORKERS: int = 10
MESSAGES_PER_WORKER: int = 5


@pytest.mark.integration
class TestWebSocketConcurrency:
    def test_rapid_fire(self, ws_url: str) -> None:
        """Send many messages without waiting, then verify all replies in order."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                for i in range(RAPID_FIRE_COUNT):
                    await ws.send(f"rf-{i}")

                for i in range(RAPID_FIRE_COUNT):
                    reply = await asyncio.wait_for(
                        ws.recv(), timeout=RECV_TIMEOUT
                    )
                    assert reply == f"rf-{i}"

        asyncio.run(_run())

    def test_reconnect_after_close(self, ws_url: str) -> None:
        """Close, reconnect, and verify the second session works."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                assert await echo_roundtrip(ws, "session-1") == "session-1"

            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                assert await echo_roundtrip(ws, "session-2") == "session-2"

        asyncio.run(_run())

    def test_concurrent_binary(self, ws_url: str) -> None:
        """Multiple workers sending binary frames concurrently."""

        async def worker(n: int) -> None:
            async with ws_connect(ws_url, "/api/ws/binary") as ws:
                for i in range(MESSAGES_PER_WORKER):
                    data = f"w{n}-{i}".encode()
                    assert await binary_roundtrip(ws, data) == data

        async def _run() -> None:
            await asyncio.gather(
                *[worker(i) for i in range(CONCURRENT_WORKERS)]
            )

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Error recovery
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketErrorRecovery:
    def test_server_error_closes_cleanly(self, ws_url: str) -> None:
        """Server-side RuntimeError closes the WS; server stays healthy."""

        async def _run() -> None:
            async with ws_connect(ws_url, "/api/ws/error-in-handler") as ws:
                with pytest.raises(ConnectionClosed):
                    await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)

            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                assert await echo_roundtrip(ws, "post-error") == "post-error"

        asyncio.run(_run())

    def test_send_after_close(self, ws_url: str) -> None:
        """Sending on a closed connection raises ConnectionClosed."""

        async def _run() -> None:
            ws: ClientConnection
            async with ws_connect(ws_url, "/api/ws/echo") as ws:
                await echo_roundtrip(ws, "before")
                await ws.close()

            with pytest.raises(ConnectionClosed):
                await ws.send("after-close")

        asyncio.run(_run())


# ---------------------------------------------------------------------------
# Subprotocol
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestWebSocketSubprotocol:
    def test_subprotocol_negotiation(self, ws_url: str) -> None:
        """Server receives offered subprotocols and echoes the selected one."""

        async def _run() -> None:
            async with ws_connect(
                ws_url,
                "/api/ws/subprotocol",
                subprotocols=["graphql-ws", "graphql-transport-ws"],
            ) as ws:
                raw = await asyncio.wait_for(ws.recv(), timeout=RECV_TIMEOUT)
                assert isinstance(raw, str)
                reply: dict[str, str] = json.loads(raw)
                assert reply["selected"] == "graphql-ws"

        asyncio.run(_run())
