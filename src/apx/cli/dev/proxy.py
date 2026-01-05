"""HTTP and WebSocket reverse proxy for the apx dev server.

This module implements a reverse proxy that routes requests:
- `/<api_prefix>/*` -> Backend server (in-process uvicorn)
- `/*` -> Frontend server (vite/bun dev server)

It supports both HTTP and WebSocket proxying with graceful shutdown.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

import httpx
from starlette.requests import Request
from starlette.responses import Response, StreamingResponse
from starlette.websockets import WebSocket, WebSocketDisconnect

from apx.cli.dev.logging import DevLogComponent, get_logger
from apx.constants import APX_DEV_PROXY_HEADER, DEFAULT_API_PREFIX

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

logger = get_logger(DevLogComponent.PROXY)


class ProxyManager:
    """Manages HTTP and WebSocket proxying with graceful shutdown support.

    Attributes:
        frontend_url: Base URL for the frontend server (e.g., "http://localhost:5000")
        backend_url: Base URL for the backend server (e.g., "http://localhost:8000")
        api_prefix: URL prefix for API routes (e.g., "/api")
        accepting_connections: Flag to control whether new connections are accepted
    """

    def __init__(
        self,
        frontend_url: str,
        backend_url: str,
        api_prefix: str = DEFAULT_API_PREFIX,
    ) -> None:
        self.frontend_url: str = frontend_url.rstrip("/")
        self.backend_url: str = backend_url.rstrip("/")
        self.api_prefix: str = api_prefix.rstrip("/")
        self.accepting_connections: bool = True

        # Track active WebSocket connections for graceful shutdown
        self._active_websockets: set[asyncio.Task[None]] = set()
        self._ws_lock: asyncio.Lock = asyncio.Lock()

        # HTTP client with connection pooling
        self._http_client: httpx.AsyncClient | None = None

    async def _get_http_client(self) -> httpx.AsyncClient:
        """Get or create the HTTP client."""
        if self._http_client is None or self._http_client.is_closed:
            self._http_client = httpx.AsyncClient(
                timeout=httpx.Timeout(60.0, connect=10.0),
                follow_redirects=False,
                limits=httpx.Limits(max_connections=100, max_keepalive_connections=20),
            )
        return self._http_client

    def _get_target_url(self, path: str) -> str:
        """Determine target URL based on path prefix."""
        if path.startswith(self.api_prefix):
            return self.backend_url
        return self.frontend_url

    def _is_api_route(self, path: str) -> bool:
        """Check if path is an API route."""
        return path.startswith(self.api_prefix)

    async def proxy_http(self, request: Request) -> Response:
        """Proxy an HTTP request to the appropriate backend.

        Args:
            request: The incoming Starlette request

        Returns:
            Response from the proxied server
        """
        if not self.accepting_connections:
            return Response(
                content="Server is shutting down",
                status_code=503,
                media_type="text/plain",
            )

        path = request.url.path
        query_string = request.url.query
        target_base = self._get_target_url(path)

        # Build target URL
        target_url = f"{target_base}{path}"
        if query_string:
            target_url = f"{target_url}?{query_string}"

        # Forward headers, excluding hop-by-hop headers
        hop_by_hop = {
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailers",
            "transfer-encoding",
            "upgrade",
        }
        headers = {
            k: v
            for k, v in request.headers.items()
            if k.lower() not in hop_by_hop and k.lower() != "host"
        }

        # Add forwarding headers
        client_host = request.client.host if request.client else "unknown"
        headers["x-forwarded-for"] = client_host
        headers["x-forwarded-proto"] = request.url.scheme
        headers["x-forwarded-host"] = request.headers.get("host", "")

        # Add dev proxy header for frontend requests (for Vite verification)
        if not self._is_api_route(path):
            headers[APX_DEV_PROXY_HEADER] = "true"

        try:
            client = await self._get_http_client()

            # Read request body
            body = await request.body()

            # Make the proxied request
            response = await client.request(
                method=request.method,
                url=target_url,
                headers=headers,
                content=body,
            )

            # Build response headers
            response_headers: dict[str, str] = {}
            for key, value in response.headers.multi_items():
                # Skip hop-by-hop headers
                if key.lower() not in hop_by_hop:
                    response_headers[key] = value

            return Response(
                content=response.content,
                status_code=response.status_code,
                headers=response_headers,
                media_type=response.headers.get("content-type"),
            )

        except httpx.ConnectError as e:
            target_name = "backend" if self._is_api_route(path) else "frontend"
            logger.warning(f"Failed to connect to {target_name}: {e}")
            return Response(
                content=f"Failed to connect to {target_name} server",
                status_code=502,
                media_type="text/plain",
            )
        except httpx.TimeoutException:
            return Response(
                content="Request timed out",
                status_code=504,
                media_type="text/plain",
            )
        except Exception as e:
            logger.error(f"Proxy error: {e}")
            return Response(
                content=f"Proxy error: {e}",
                status_code=500,
                media_type="text/plain",
            )

    async def proxy_http_streaming(self, request: Request) -> StreamingResponse:
        """Proxy an HTTP request with streaming response support.

        Useful for SSE endpoints and large file downloads.

        Args:
            request: The incoming Starlette request

        Returns:
            StreamingResponse from the proxied server
        """
        if not self.accepting_connections:
            return StreamingResponse(
                content=iter([b"Server is shutting down"]),
                status_code=503,
                media_type="text/plain",
            )

        path = request.url.path
        query_string = request.url.query
        target_base = self._get_target_url(path)

        target_url = f"{target_base}{path}"
        if query_string:
            target_url = f"{target_url}?{query_string}"

        hop_by_hop = {
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailers",
            "transfer-encoding",
            "upgrade",
        }
        headers = {
            k: v
            for k, v in request.headers.items()
            if k.lower() not in hop_by_hop and k.lower() != "host"
        }

        client_host = request.client.host if request.client else "unknown"
        headers["x-forwarded-for"] = client_host
        headers["x-forwarded-proto"] = request.url.scheme
        headers["x-forwarded-host"] = request.headers.get("host", "")

        # Add dev proxy header for frontend requests (for Vite verification)
        if not self._is_api_route(path):
            headers[APX_DEV_PROXY_HEADER] = "true"

        try:
            client = await self._get_http_client()
            body = await request.body()

            # Use stream context for streaming response
            req = client.build_request(
                method=request.method,
                url=target_url,
                headers=headers,
                content=body,
            )

            response = await client.send(req, stream=True)

            response_headers: dict[str, str] = {}
            for key, value in response.headers.multi_items():
                if key.lower() not in hop_by_hop:
                    response_headers[key] = value

            async def stream_response() -> AsyncIterator[bytes]:
                try:
                    async for chunk in response.aiter_bytes():
                        yield chunk
                finally:
                    await response.aclose()

            return StreamingResponse(
                content=stream_response(),
                status_code=response.status_code,
                headers=response_headers,
                media_type=response.headers.get("content-type"),
            )

        except httpx.ConnectError as e:
            target_name = "backend" if self._is_api_route(path) else "frontend"
            logger.warning(f"Failed to connect to {target_name}: {e}")
            error_msg = f"Failed to connect to {target_name} server"

            async def connect_error_stream() -> AsyncIterator[bytes]:
                yield error_msg.encode()

            return StreamingResponse(
                content=connect_error_stream(),
                status_code=502,
                media_type="text/plain",
            )
        except Exception as exc:
            logger.error(f"Streaming proxy error: {exc}")
            error_msg = f"Proxy error: {exc}"

            async def generic_error_stream() -> AsyncIterator[bytes]:
                yield error_msg.encode()

            return StreamingResponse(
                content=generic_error_stream(),
                status_code=500,
                media_type="text/plain",
            )

    async def proxy_websocket(self, websocket: WebSocket) -> None:
        """Proxy a WebSocket connection to the appropriate backend.

        Args:
            websocket: The incoming Starlette WebSocket connection
        """
        if not self.accepting_connections:
            await websocket.close(code=1001, reason="Server is shutting down")
            return

        path = websocket.url.path
        query_string = websocket.url.query
        target_base = self._get_target_url(path)

        # Convert http:// to ws://
        ws_target = target_base.replace("http://", "ws://").replace(
            "https://", "wss://"
        )
        target_url = f"{ws_target}{path}"
        if query_string:
            target_url = f"{target_url}?{query_string}"

        # Accept the client WebSocket connection first
        await websocket.accept()

        # Import websockets library
        try:
            import websockets
            from websockets.asyncio.client import connect as ws_connect
        except ImportError:
            logger.error("websockets library not installed")
            await websocket.close(code=1011, reason="WebSocket proxy not available")
            return

        from websockets.asyncio.client import ClientConnection

        target_ws: ClientConnection | None = None
        forward_task: asyncio.Task[None] | None = None
        backward_task: asyncio.Task[None] | None = None

        try:
            # Connect to target WebSocket server
            target_ws = await ws_connect(target_url)
            # Store reference for nested functions
            active_target_ws = target_ws

            async def forward_to_target() -> None:
                """Forward messages from client to target."""
                try:
                    while True:
                        data = await websocket.receive()
                        if data["type"] == "websocket.disconnect":
                            break
                        if "text" in data:
                            await active_target_ws.send(data["text"])
                        elif "bytes" in data:
                            await active_target_ws.send(data["bytes"])
                except WebSocketDisconnect:
                    pass
                except Exception as e:
                    logger.debug(f"Forward to target error: {e}")

            async def forward_to_client() -> None:
                """Forward messages from target to client."""
                try:
                    async for message in active_target_ws:
                        if isinstance(message, str):
                            await websocket.send_text(message)
                        else:
                            await websocket.send_bytes(message)
                except websockets.exceptions.ConnectionClosed:
                    pass
                except Exception as e:
                    logger.debug(f"Forward to client error: {e}")

            # Register this connection
            current_task = asyncio.current_task()
            if current_task:
                async with self._ws_lock:
                    self._active_websockets.add(current_task)

            # Run both directions concurrently
            forward_task = asyncio.create_task(forward_to_target())
            backward_task = asyncio.create_task(forward_to_client())

            # Wait for either direction to complete
            done, pending = await asyncio.wait(
                [forward_task, backward_task],
                return_when=asyncio.FIRST_COMPLETED,
            )

            # Cancel pending tasks
            for task in pending:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass

        except Exception as e:
            logger.warning(f"WebSocket proxy error: {e}")
        finally:
            # Unregister this connection
            current_task = asyncio.current_task()
            if current_task:
                async with self._ws_lock:
                    self._active_websockets.discard(current_task)

            # Close connections
            if target_ws is not None:
                try:
                    await target_ws.close()
                except Exception:
                    pass

            try:
                await websocket.close()
            except Exception:
                pass

    async def shutdown(self, timeout: float = 5.0) -> None:
        """Gracefully shutdown the proxy.

        1. Stop accepting new connections
        2. Close all active WebSocket connections
        3. Close the HTTP client

        Args:
            timeout: Maximum time to wait for connections to close
        """
        logger.info("Shutting down proxy...")
        self.accepting_connections = False

        # Close all active WebSocket connections
        async with self._ws_lock:
            tasks = list(self._active_websockets)

        if tasks:
            logger.info(f"Closing {len(tasks)} active WebSocket connection(s)...")
            for task in tasks:
                task.cancel()

            # Wait for tasks to complete with timeout
            try:
                await asyncio.wait_for(
                    asyncio.gather(*tasks, return_exceptions=True),
                    timeout=timeout,
                )
            except asyncio.TimeoutError:
                logger.warning("Timeout waiting for WebSocket connections to close")

        # Close HTTP client
        if self._http_client and not self._http_client.is_closed:
            await self._http_client.aclose()
            self._http_client = None

        logger.info("Proxy shutdown complete")

    @property
    def active_websocket_count(self) -> int:
        """Return the number of active WebSocket connections."""
        return len(self._active_websockets)
