"""Tests for the proxy module."""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING
from unittest.mock import AsyncMock, Mock, patch

import httpx
import pytest
from starlette.requests import Request
from starlette.websockets import WebSocket

from apx.cli.dev.proxy import ProxyManager
from apx.constants import APX_DEV_PROXY_HEADER

if TYPE_CHECKING:
    from collections.abc import AsyncIterator


@pytest.fixture
def proxy() -> ProxyManager:
    return ProxyManager(
        frontend_url="http://localhost:5000/",
        backend_url="http://localhost:8000/",
        api_prefix="/api/",
    )


@pytest.fixture
def mock_request() -> Mock:
    req = Mock(spec=Request)
    req.url.path = "/"
    req.url.query = ""
    req.url.scheme = "http"
    req.method = "GET"
    req.headers = {"user-agent": "test"}
    req.client.host = "127.0.0.1"
    req.body = AsyncMock(return_value=b"")
    return req


@pytest.fixture
def mock_response() -> Mock:
    resp = Mock()
    resp.content = b"ok"
    resp.status_code = 200
    resp.headers = httpx.Headers({"content-type": "text/plain"})
    resp.headers.multi_items = lambda: [("content-type", "text/plain")]
    return resp


class TestProxyManager:
    """Tests for ProxyManager initialization, routing, and HTTP client."""

    def test_init_and_routing(self, proxy: ProxyManager) -> None:
        assert proxy.frontend_url == "http://localhost:5000"
        assert proxy.backend_url == "http://localhost:8000"
        assert proxy.api_prefix == "/api"
        assert proxy._get_target_url("/api/users") == "http://localhost:8000"
        assert proxy._get_target_url("/") == "http://localhost:5000"
        assert proxy._is_api_route("/api/users") is True
        assert proxy._is_api_route("/about") is False

    @pytest.mark.asyncio
    async def test_http_client_lifecycle(self, proxy: ProxyManager) -> None:
        client1 = await proxy._get_http_client()
        assert client1 is await proxy._get_http_client()
        await client1.aclose()
        assert client1 is not await proxy._get_http_client()


class TestProxyHttp:
    """Tests for HTTP proxying."""

    @pytest.mark.asyncio
    async def test_frontend_proxy(
        self, proxy: ProxyManager, mock_request: Mock, mock_response: Mock
    ) -> None:
        mock_request.headers = {"connection": "keep-alive", "x-custom": "val"}
        with patch.object(proxy, "_get_http_client") as m:
            m.return_value = AsyncMock(request=AsyncMock(return_value=mock_response))
            resp = await proxy.proxy_http(mock_request)
            assert resp.status_code == 200
            headers = m.return_value.request.call_args.kwargs["headers"]
            assert "connection" not in headers
            assert APX_DEV_PROXY_HEADER in headers

    @pytest.mark.asyncio
    async def test_backend_proxy_with_query(
        self, proxy: ProxyManager, mock_request: Mock, mock_response: Mock
    ) -> None:
        mock_request.url.path = "/api/users"
        mock_request.url.query = "page=1"
        with patch.object(proxy, "_get_http_client") as m:
            m.return_value = AsyncMock(request=AsyncMock(return_value=mock_response))
            await proxy.proxy_http(mock_request)
            url = m.return_value.request.call_args.kwargs["url"]
            assert url == "http://localhost:8000/api/users?page=1"
            assert (
                APX_DEV_PROXY_HEADER
                not in m.return_value.request.call_args.kwargs["headers"]
            )

    @pytest.mark.asyncio
    async def test_error_handling(
        self, proxy: ProxyManager, mock_request: Mock
    ) -> None:
        for error, code in [
            (httpx.ConnectError(""), 502),
            (httpx.TimeoutException(""), 504),
            (ValueError(""), 500),
        ]:
            with patch.object(proxy, "_get_http_client") as m:
                m.return_value = AsyncMock(request=AsyncMock(side_effect=error))
                assert (await proxy.proxy_http(mock_request)).status_code == code

    @pytest.mark.asyncio
    async def test_shutdown_and_no_client(
        self, proxy: ProxyManager, mock_request: Mock, mock_response: Mock
    ) -> None:
        proxy.accepting_connections = False
        assert (await proxy.proxy_http(mock_request)).status_code == 503
        proxy.accepting_connections = True
        mock_request.client = None
        with patch.object(proxy, "_get_http_client") as m:
            m.return_value = AsyncMock(request=AsyncMock(return_value=mock_response))
            await proxy.proxy_http(mock_request)
            assert (
                m.return_value.request.call_args.kwargs["headers"]["x-forwarded-for"]
                == "unknown"
            )


class TestProxyHttpStreaming:
    """Tests for streaming HTTP proxying."""

    @pytest.mark.asyncio
    async def test_streaming_success(
        self, proxy: ProxyManager, mock_request: Mock
    ) -> None:
        async def mock_iter() -> AsyncIterator[bytes]:
            yield b"chunk"

        mock_resp = AsyncMock()
        mock_resp.status_code = 200
        mock_resp.headers = httpx.Headers(
            {"content-type": "text/event-stream", "connection": "keep-alive"}
        )
        mock_resp.headers.multi_items = lambda: [
            ("content-type", "text/event-stream"),
            ("connection", "keep-alive"),
        ]
        mock_resp.aiter_bytes = mock_iter
        mock_resp.aclose = AsyncMock()

        with patch.object(proxy, "_get_http_client") as m:
            m.return_value = AsyncMock(
                build_request=Mock(return_value=Mock()),
                send=AsyncMock(return_value=mock_resp),
            )
            resp = await proxy.proxy_http_streaming(mock_request)
            chunks = [c async for c in resp.body_iterator]
            assert resp.status_code == 200
            assert chunks == [b"chunk"]
            assert "connection" not in resp.headers

    @pytest.mark.asyncio
    async def test_streaming_errors(
        self, proxy: ProxyManager, mock_request: Mock
    ) -> None:
        mock_request.url.query = "q=1"
        for error, code in [(httpx.ConnectError(""), 502), (ValueError(""), 500)]:
            with patch.object(proxy, "_get_http_client") as m:
                client = AsyncMock()
                client.build_request = (
                    Mock(return_value=Mock())
                    if isinstance(error, httpx.ConnectError)
                    else Mock(side_effect=error)
                )
                client.send = (
                    AsyncMock(side_effect=error)
                    if isinstance(error, httpx.ConnectError)
                    else AsyncMock()
                )
                m.return_value = client
                resp = await proxy.proxy_http_streaming(mock_request)
                async for _ in resp.body_iterator:
                    pass
                assert resp.status_code == code

    @pytest.mark.asyncio
    async def test_streaming_shutdown(
        self, proxy: ProxyManager, mock_request: Mock
    ) -> None:
        proxy.accepting_connections = False
        assert (await proxy.proxy_http_streaming(mock_request)).status_code == 503


class TestProxyWebSocket:
    """Tests for WebSocket proxying."""

    @pytest.mark.asyncio
    async def test_shutdown_and_errors(self, proxy: ProxyManager) -> None:
        mock_ws = AsyncMock(spec=WebSocket)
        proxy.accepting_connections = False
        await proxy.proxy_websocket(mock_ws)
        mock_ws.close.assert_called_with(code=1001, reason="Server is shutting down")

        proxy.accepting_connections = True
        mock_ws.reset_mock()
        mock_ws.url.path = "/ws"
        mock_ws.url.query = ""
        with patch("websockets.asyncio.client.connect", side_effect=Exception("fail")):
            await proxy.proxy_websocket(mock_ws)
        mock_ws.accept.assert_called_once()
        mock_ws.close.assert_called()

    @pytest.mark.asyncio
    async def test_import_error(self, proxy: ProxyManager) -> None:
        mock_ws = AsyncMock(spec=WebSocket)
        mock_ws.url.path = "/ws"
        mock_ws.url.query = ""
        with patch.dict("sys.modules", {"websockets": None}):
            with patch("builtins.__import__", side_effect=ImportError()):
                await proxy.proxy_websocket(mock_ws)
        mock_ws.close.assert_called_with(
            code=1011, reason="WebSocket proxy not available"
        )


class TestProxyShutdown:
    """Tests for graceful shutdown."""

    @pytest.mark.asyncio
    async def test_shutdown(self, proxy: ProxyManager) -> None:
        await proxy._get_http_client()
        task = asyncio.create_task(asyncio.sleep(10))
        proxy._active_websockets.add(task)
        await proxy.shutdown(timeout=1.0)
        assert proxy.accepting_connections is False
        assert proxy._http_client is None
        assert task.cancelled()

    @pytest.mark.asyncio
    async def test_shutdown_timeout(self, proxy: ProxyManager) -> None:
        async def stubborn() -> None:
            try:
                await asyncio.sleep(100)
            except asyncio.CancelledError:
                await asyncio.sleep(100)

        task = asyncio.create_task(stubborn())
        proxy._active_websockets.add(task)
        await proxy.shutdown(timeout=0.05)
        assert proxy.accepting_connections is False
        task.cancel()
        try:
            await asyncio.wait_for(task, timeout=0.1)
        except (asyncio.CancelledError, asyncio.TimeoutError):
            pass

    @pytest.mark.asyncio
    async def test_websocket_count(self, proxy: ProxyManager) -> None:
        assert proxy.active_websocket_count == 0
        task = asyncio.create_task(asyncio.sleep(0.1))
        proxy._active_websockets.add(task)
        assert proxy.active_websocket_count == 1
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
