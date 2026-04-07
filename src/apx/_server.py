"""ASGI server using Rust-accelerated protocol and inline driving.

Uses asyncio ``loop.create_server()`` with Rust HTTP parsing, scope
building, and response writing.
"""

from __future__ import annotations

import asyncio
import logging
import traceback
from collections.abc import Callable, Coroutine
from typing import Any

from apx._continuation import Continuation
from apx._core import LifespanReceive, LifespanSend
from apx._websocket import WebSocketBridge, build_upgrade_response
from apx._scheduler import (
    CallSoonCapture,
    Completed,
    Failed,
    SchedulerTask,
    Suspended,
    drive_inline,
)

logger = logging.getLogger("apx.server")

LIFESPAN_TIMEOUT = 30.0


async def _guarded(
    app: Callable[..., Coroutine[Any, Any, None]],
    scope: dict[str, Any],
    receive: Any,
    send: Any,
) -> None:
    """Wrap the ASGI app call with error handling."""
    try:
        await app(scope, receive, send)
    except Exception:
        tb = traceback.format_exc()
        logger.error("ASGI application error:\n%s", tb)
        try:
            send.send_error(tb)
        except Exception:
            logger.error("Failed to send error response:\n%s", traceback.format_exc())


def _build_on_request(
    app: Callable[..., Coroutine[Any, Any, None]],
    loop: asyncio.AbstractEventLoop,
    capture: CallSoonCapture,
) -> Callable[..., None]:
    """Build the on_request dispatch callback with inline driving."""

    def on_request(
        scope: dict[str, Any],
        receive: Any,
        send: Any,
    ) -> None:
        coro = _guarded(app, scope, receive, send)
        task = SchedulerTask(loop=loop)
        capture.enter()
        try:
            result = drive_inline(coro, task, loop, capture)
        except BaseException:
            coro.close()
            capture.leave()
            raise
        capture.leave()
        if isinstance(result, Completed):
            return
        if isinstance(result, Failed):
            try:
                send.send_error(traceback.format_exc())
            except Exception:
                logger.error("Failed to send error: %s", traceback.format_exc())
            return
        if isinstance(result, Suspended):
            Continuation(coro, result.yielded, loop, task, capture)

    return on_request


def _build_on_ws_connect(
    app: Callable[..., Coroutine[Any, Any, None]],
) -> Callable[..., None]:
    """Build the on_ws_connect callback for WebSocket upgrades.

    Called from Rust when an HTTP Upgrade: websocket request is detected.
    Writes the 101 Switching Protocols response, creates a WebSocketBridge,
    and stores it on the protocol so subsequent data_received calls route
    to the wsproto parser.
    """

    def on_ws_connect(
        scope: dict[str, Any],
        transport: Any,
        ws_key: str,
        protocol: Any,
    ) -> None:
        logger.debug("WebSocket upgrade: key=%r len=%d", ws_key, len(ws_key))
        # Write the 101 Switching Protocols response.
        response = build_upgrade_response(ws_key)
        logger.debug("101 response: %r", response[:100])
        transport.write(response)

        # Create the bridge and store it on the protocol.
        bridge = WebSocketBridge(transport, scope, app)
        protocol.set_ws_bridge(bridge)

        # Start the ASGI WebSocket lifecycle as an asyncio task.
        bridge.start()

    return on_ws_connect


async def _run_lifespan(
    app: Callable[..., Coroutine[Any, Any, None]],
    shutdown_event: asyncio.Event,
) -> tuple[asyncio.Task[None], str]:
    """Run ASGI lifespan startup; return (task, result_str).

    result_str is "complete", "failed:<msg>", or "unsupported".
    """
    startup_event = asyncio.Event()
    startup_result: list[str | None] = [None]
    shutdown_done_event = asyncio.Event()
    shutdown_result: list[str | None] = [None]

    receive = LifespanReceive(shutdown_event)
    send = LifespanSend(
        startup_event, startup_result, shutdown_done_event, shutdown_result
    )

    scope: dict[str, Any] = {
        "type": "lifespan",
        "asgi": {"version": "3.0", "spec_version": "2.4"},
        "state": {},
    }

    lifespan_task = asyncio.create_task(
        _guarded_lifespan(app, scope, receive, send, startup_event, startup_result)
    )

    try:
        await asyncio.wait_for(startup_event.wait(), timeout=LIFESPAN_TIMEOUT)
    except asyncio.TimeoutError:
        lifespan_task.cancel()
        raise RuntimeError("ASGI lifespan startup timed out") from None

    result = startup_result[0] or "unsupported"
    return lifespan_task, result


async def _guarded_lifespan(
    app: Callable[..., Coroutine[Any, Any, None]],
    scope: dict[str, Any],
    receive: Any,
    send: Any,
    startup_event: asyncio.Event,
    startup_result: list[str | None],
) -> None:
    """Run the ASGI lifespan protocol with error handling."""
    try:
        await app(scope, receive, send)
    except Exception:
        tb = traceback.format_exc()
        logger.warning("ASGI lifespan not supported or errored:\n%s", tb)
        if startup_result[0] is None:
            startup_result[0] = "unsupported"
            startup_event.set()


async def serve(
    host: str,
    port: int,
    app: Callable[..., Coroutine[Any, Any, None]],
    protocol_factory: Any,
    *,
    shutdown_event: asyncio.Event | None = None,
) -> None:
    """Run the ASGI server.

    Parameters
    ----------
    host:
        Bind address.
    port:
        Bind port.
    app:
        ASGI application callable.
    protocol_factory:
        Rust ``ProtocolFactory`` (callable returning ``RustProtocol``).
    shutdown_event:
        When set, the server will initiate graceful shutdown.
    """
    if shutdown_event is None:
        shutdown_event = asyncio.Event()

    lifespan_task, lifespan_result = await _run_lifespan(app, shutdown_event)

    if lifespan_result.startswith("failed"):
        msg = lifespan_result.removeprefix("failed:")
        lifespan_task.cancel()
        raise RuntimeError(f"ASGI lifespan startup failed: {msg}")

    logger.debug("lifespan startup: %s", lifespan_result)

    server = await asyncio.get_running_loop().create_server(
        protocol_factory,
        host,
        port,
        reuse_port=True,
    )

    async with server:
        await shutdown_event.wait()
        server.close()
        await server.wait_closed()

    shutdown_event.set()
    try:
        await asyncio.wait_for(lifespan_task, timeout=LIFESPAN_TIMEOUT)
    except asyncio.TimeoutError:
        logger.warning("lifespan shutdown timed out, cancelling")
        lifespan_task.cancel()
