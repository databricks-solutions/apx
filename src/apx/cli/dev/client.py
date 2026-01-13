"""HTTP client for communicating with the dev server over TCP."""

import json
from collections.abc import AsyncIterator, Iterator
from contextlib import contextmanager
from typing import Literal

import httpx
from pydantic import ValidationError

from apx.models import (
    ActionRequest,
    ActionResponse,
    LogEntry,
    StatusResponse,
    StreamEvent,
)


class DevServerClient:
    """Client for communicating with the dev server over TCP.

    The dev server now listens on localhost:<port> instead of a Unix domain socket.
    All management endpoints are under the /__apx__/ prefix.
    """

    def __init__(
        self, base_url: str | None = None, port: int | None = None, timeout: float = 5.0
    ):
        """Initialize the dev server client.

        Args:
            base_url: Full base URL (e.g., "http://localhost:7000"). If provided, port is ignored.
            port: Port number for localhost connection (e.g., 7000). Used if base_url is None.
            timeout: Default timeout for requests in seconds
        """
        if base_url:
            self.base_url: str = base_url.rstrip("/")
        elif port:
            self.base_url = f"http://localhost:{port}"
        else:
            raise ValueError("Either base_url or port must be provided")

        self.timeout: float = timeout

    @classmethod
    def from_port(cls, port: int, timeout: float = 5.0) -> "DevServerClient":
        """Create a client from a port number.

        Args:
            port: Port number for the dev server
            timeout: Default timeout for requests in seconds

        Returns:
            DevServerClient instance
        """
        return cls(port=port, timeout=timeout)

    def _management_url(self, path: str) -> str:
        """Build URL for management endpoints."""
        if not path.startswith("/"):
            path = f"/{path}"
        return f"{self.base_url}/__apx__{path}"

    def start(self, request: ActionRequest) -> ActionResponse:
        """Start the development servers.

        Args:
            request: Action request with server configuration

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=self.timeout) as client:
            response = client.post(
                self._management_url("/actions/start"),
                json=request.model_dump(),
            )
            response.raise_for_status()
            return ActionResponse.model_validate(response.json())

    def stop(self) -> ActionResponse:
        """Stop the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails (except for connection errors
                during shutdown, which are treated as success)
        """
        timeout = max(self.timeout, 15.0)
        try:
            with httpx.Client(timeout=timeout) as client:
                response = client.post(self._management_url("/actions/stop"))
                response.raise_for_status()
                return ActionResponse.model_validate(response.json())
        except (
            httpx.RemoteProtocolError,
            httpx.ReadError,
            httpx.ConnectError,
            ConnectionResetError,
        ):
            # Server disconnected during shutdown - this is expected behavior.
            # The stop request was received and the server is terminating.
            return ActionResponse(
                status="success",
                message="Server shutdown initiated (connection closed)",
            )

    def restart(self) -> ActionResponse:
        """Restart the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=10.0) as client:
            response = client.post(self._management_url("/actions/restart"))
            response.raise_for_status()
            return ActionResponse.model_validate(response.json())

    def status(self) -> StatusResponse:
        """Get the status of development servers.

        Returns:
            StatusResponse with current server status

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=self.timeout) as client:
            response = client.get(self._management_url("/status"))
            response.raise_for_status()
            return StatusResponse.model_validate(response.json())

    def is_running(self) -> bool:
        """Check if the dev server is running and responding.

        Returns:
            True if the server is running and responding, False otherwise
        """
        try:
            with httpx.Client(timeout=2.0) as client:
                response = client.get(self._management_url("/"))
                return response.status_code == 200
        except Exception:
            return False

    def get_logs_snapshot(
        self,
        *,
        duration: int | None = None,
        channel: Literal["app", "ui", "apx", "all"] = "all",
        component: str | None = None,
        include_system: bool = False,
        limit: int = 500,
    ) -> list[LogEntry]:
        """Fetch a bounded snapshot of dev logs (non-streaming)."""
        params: dict[str, str] = {"channel": channel, "limit": str(limit)}
        if duration is not None:
            params["duration"] = str(duration)
        if component is not None and component.strip():
            params["component"] = component.strip()
        if include_system:
            params["include_system"] = "true"

        with httpx.Client(timeout=self.timeout) as client:
            resp = client.get(self._management_url("/logs/snapshot"), params=params)
            resp.raise_for_status()
            data = resp.json()
            if not isinstance(data, list):
                raise ValueError("Expected a JSON list of log entries")
            return [LogEntry.model_validate(item) for item in data]

    @contextmanager
    def stream_logs(
        self,
        duration: int | None = None,
        channel: Literal["app", "ui", "apx", "all"] = "all",
        component: str | None = None,
        include_system: bool = False,
    ) -> Iterator[Iterator[LogEntry | StreamEvent]]:
        """Stream logs from the dev server using Server-Sent Events.

        This method returns a context manager that yields an iterator of LogEntry objects
        and StreamEvent markers. The SSE connection will be automatically closed when
        exiting the context.

        The iterator will yield:
        - LogEntry objects for each log entry
        - StreamEvent.BUFFERED_DONE marker when all buffered logs have been sent

        Args:
            duration: Show logs from last N seconds (None = all logs from buffer)
            channel: Filter by log channel
            component: Filter by component (optional)
            include_system: Include system [apx] logs (only applies when channel=all)

        Yields:
            Iterator of LogEntry objects and StreamEvent markers from the SSE stream

        Raises:
            httpx.HTTPError: If the request fails

        Example:
            >>> client = DevServerClient(port=7000)
            >>> with client.stream_logs() as log_stream:
            ...     for item in log_stream:
            ...         if isinstance(item, LogEntry):
            ...             print(item.content)
            ...         elif item == StreamEvent.BUFFERED_DONE:
            ...             break  # Stop after buffered logs
        """
        params: dict[str, str] = {"channel": channel}
        if duration is not None:
            params["duration"] = str(duration)
        if component is not None and component.strip():
            params["component"] = component.strip()
        if include_system:
            params["include_system"] = "true"

        with httpx.Client(timeout=None) as client:
            with client.stream(
                "GET", self._management_url("/logs"), params=params
            ) as response:
                response.raise_for_status()

                def log_iterator() -> Iterator[LogEntry | StreamEvent]:
                    """Parse SSE events and yield LogEntry objects and markers."""
                    skip_next_data = False

                    try:
                        for line in response.iter_lines():
                            if not line:
                                continue

                            # Check for server shutdown event
                            if line.startswith("event: server_shutdown"):
                                skip_next_data = True
                                yield StreamEvent.SERVER_SHUTDOWN
                                return  # Exit immediately on server shutdown

                            # Check for sentinel event marking end of buffered logs
                            if line.startswith("event: buffered_done"):
                                skip_next_data = True
                                yield StreamEvent.BUFFERED_DONE
                                continue

                            # Parse SSE data lines
                            if line.startswith("data: "):
                                if skip_next_data:
                                    skip_next_data = False
                                    continue

                                data_str = line[6:]  # Remove "data: " prefix
                                try:
                                    yield LogEntry.model_validate(json.loads(data_str))
                                except ValidationError:
                                    continue
                    except (
                        httpx.RemoteProtocolError,
                        httpx.ReadError,
                        httpx.ConnectError,
                    ):
                        # Server shut down or connection was closed - exit gracefully
                        return

                yield log_iterator()

    async def stream_logs_async(
        self,
        duration: int | None = None,
        channel: Literal["app", "ui", "apx", "all"] = "all",
        component: str | None = None,
        include_system: bool = False,
    ) -> AsyncIterator[LogEntry | StreamEvent]:
        """Async version of stream_logs for use in async contexts.

        Args:
            duration: Show logs from last N seconds (None = all logs from buffer)
            channel: Filter by log channel
            component: Filter by component (optional)
            include_system: Include system [apx] logs (only applies when channel=all)

        Yields:
            LogEntry objects and StreamEvent markers from the SSE stream

        Raises:
            httpx.HTTPError: If the request fails
        """
        params: dict[str, str] = {"channel": channel}
        if duration is not None:
            params["duration"] = str(duration)
        if component is not None and component.strip():
            params["component"] = component.strip()
        if include_system:
            params["include_system"] = "true"

        async with httpx.AsyncClient(timeout=None) as client:
            async with client.stream(
                "GET", self._management_url("/logs"), params=params
            ) as response:
                response.raise_for_status()

                skip_next_data = False

                try:
                    async for line in response.aiter_lines():
                        if not line:
                            continue

                        # Check for server shutdown event
                        if line.startswith("event: server_shutdown"):
                            skip_next_data = True
                            yield StreamEvent.SERVER_SHUTDOWN
                            return  # Exit immediately on server shutdown

                        if line.startswith("event: buffered_done"):
                            skip_next_data = True
                            yield StreamEvent.BUFFERED_DONE
                            continue

                        if line.startswith("data: "):
                            if skip_next_data:
                                skip_next_data = False
                                continue

                            data_str = line[6:]
                            try:
                                yield LogEntry.model_validate(json.loads(data_str))
                            except ValidationError:
                                continue
                except (httpx.RemoteProtocolError, httpx.ReadError, httpx.ConnectError):
                    # Server shut down or connection was closed - exit gracefully
                    return
