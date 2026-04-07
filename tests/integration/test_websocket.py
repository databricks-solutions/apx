"""WebSocket integration tests for APX.

Tests run against the bench app's WebSocket endpoints (ws/echo, ws/json)
inside the Docker container managed by the session fixtures.
"""

from __future__ import annotations

import asyncio
import json

import pytest
import websockets


@pytest.fixture(scope="session")
def ws_url(apx_container: str) -> str:
    """WebSocket base URL derived from the HTTP container URL."""
    return apx_container.replace("http://", "ws://")


@pytest.mark.integration
class TestWebSocketEcho:
    def test_single_message(self, ws_url: str) -> None:
        async def _run():
            async with websockets.connect(f"{ws_url}/api/ws/echo") as ws:
                await ws.send("hello")
                reply = await asyncio.wait_for(ws.recv(), timeout=5)
                assert reply == "hello"

        asyncio.run(_run())

    def test_multiple_messages(self, ws_url: str) -> None:
        async def _run():
            async with websockets.connect(f"{ws_url}/api/ws/echo") as ws:
                for i in range(10):
                    await ws.send(f"msg-{i}")
                    reply = await asyncio.wait_for(ws.recv(), timeout=5)
                    assert reply == f"msg-{i}"

        asyncio.run(_run())

    def test_concurrent_connections(self, ws_url: str) -> None:
        async def worker(n: int) -> None:
            async with websockets.connect(f"{ws_url}/api/ws/echo") as ws:
                for i in range(5):
                    msg = f"w{n}-{i}"
                    await ws.send(msg)
                    reply = await asyncio.wait_for(ws.recv(), timeout=5)
                    assert reply == msg

        async def _run():
            await asyncio.gather(*[worker(i) for i in range(10)])

        asyncio.run(_run())

    def test_client_disconnect(self, ws_url: str) -> None:
        """Server handles client disconnect gracefully."""

        async def _run():
            async with websockets.connect(f"{ws_url}/api/ws/echo") as ws:
                await ws.send("hello")
                await ws.recv()
            # Connection closed — no crash expected.

        asyncio.run(_run())


@pytest.mark.integration
class TestWebSocketJSON:
    def test_json_echo(self, ws_url: str) -> None:
        async def _run():
            async with websockets.connect(f"{ws_url}/api/ws/json") as ws:
                payload = {"action": "ping", "n": 42}
                await ws.send(json.dumps(payload))
                reply = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
                assert reply["action"] == "ping"
                assert reply["n"] == 42
                assert "server_ts" in reply

        asyncio.run(_run())
