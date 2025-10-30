"""HTTP client for communicating with the dev server using httpx over Unix domain socket."""

import json
from collections.abc import AsyncIterator, Iterator
from contextlib import contextmanager
from enum import Enum
from pathlib import Path
from typing import Literal

import httpx
from pydantic import ValidationError

from apx.cli.dev.models import ActionRequest, ActionResponse, LogEntry, StatusResponse


class StreamEvent(str, Enum):
    """Marker for special SSE events."""

    BUFFERED_DONE = "buffered_done"


class DevServerClient:
    """Client for communicating with the dev server over Unix domain socket."""

    def __init__(self, socket_path: Path | str, timeout: float = 5.0):
        """Initialize the dev server client.

        Args:
            socket_path: Path to Unix domain socket (e.g., ".apx/dev.sock")
            timeout: Default timeout for requests in seconds
        """
        if isinstance(socket_path, str):
            socket_path = Path(socket_path)

        self.socket_path: Path = socket_path
        self.timeout: float = timeout
        # Use a custom transport for Unix domain sockets
        self.transport: httpx.HTTPTransport = httpx.HTTPTransport(uds=str(socket_path))
        # Base URL doesn't matter for Unix sockets, but httpx needs one
        self.base_url: str = "http://localhost"

    def start(self, request: ActionRequest) -> ActionResponse:
        """Start the development servers.

        Args:
            request: Action request with server configuration

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(transport=self.transport, timeout=self.timeout) as client:
            response = client.post(
                f"{self.base_url}/actions/start",
                json=request.model_dump(),
            )
            response.raise_for_status()
            return ActionResponse.model_validate(response.json())

    def stop(self) -> ActionResponse:
        """Stop the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(transport=self.transport, timeout=self.timeout) as client:
            response = client.post(f"{self.base_url}/actions/stop")
            response.raise_for_status()
            return ActionResponse.model_validate(response.json())

    def restart(self) -> ActionResponse:
        """Restart the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        transport = httpx.HTTPTransport(uds=str(self.socket_path))
        with httpx.Client(
            transport=transport, timeout=10.0
        ) as client:  # Longer timeout for restart
            response = client.post(f"{self.base_url}/actions/restart")
            response.raise_for_status()
            return ActionResponse.model_validate(response.json())

    def status(self) -> StatusResponse:
        """Get the status of development servers.

        Returns:
            StatusResponse with current server status

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(transport=self.transport, timeout=self.timeout) as client:
            response = client.get(f"{self.base_url}/status")
            response.raise_for_status()
            return StatusResponse.model_validate(response.json())

    @contextmanager
    def stream_logs(
        self,
        duration: int | None = None,
        process: Literal["frontend", "backend", "openapi", "all"] = "all",
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
            process: Filter by process name

        Yields:
            Iterator of LogEntry objects and StreamEvent markers from the SSE stream

        Raises:
            httpx.HTTPError: If the request fails

        Example:
            >>> client = DevServerClient("http://localhost:8040")
            >>> with client.stream_logs() as log_stream:
            ...     for item in log_stream:
            ...         if isinstance(item, LogEntry):
            ...             print(item.content)
            ...         elif item == StreamEvent.BUFFERED_DONE:
            ...             break  # Stop after buffered logs
        """
        params: dict[str, str] = {"process": process}
        if duration is not None:
            params["duration"] = str(duration)

        transport = httpx.HTTPTransport(uds=str(self.socket_path))
        with httpx.Client(transport=transport, timeout=None) as client:
            with client.stream(
                "GET", f"{self.base_url}/logs", params=params
            ) as response:
                response.raise_for_status()

                def log_iterator() -> Iterator[LogEntry | StreamEvent]:
                    """Parse SSE events and yield LogEntry objects and markers."""
                    skip_next_data = False

                    for line in response.iter_lines():
                        if not line:
                            continue

                        # Check for sentinel event marking end of buffered logs
                        if line.startswith("event: buffered_done"):
                            skip_next_data = True
                            yield StreamEvent.BUFFERED_DONE
                            continue

                        # Parse SSE data lines
                        if line.startswith("data: "):
                            # Skip the data line that comes after buffered_done event
                            if skip_next_data:
                                skip_next_data = False
                                continue

                            data_str = line[6:]  # Remove "data: " prefix
                            try:
                                yield LogEntry.model_validate(json.loads(data_str))
                            except ValidationError:
                                continue

                yield log_iterator()

    async def stream_logs_async(
        self,
        duration: int | None = None,
        process: Literal["frontend", "backend", "openapi", "all"] = "all",
    ) -> AsyncIterator[LogEntry | StreamEvent]:
        """Async version of stream_logs for use in async contexts.

        Args:
            duration: Show logs from last N seconds (None = all logs from buffer)
            process: Filter by process name

        Yields:
            LogEntry objects and StreamEvent markers from the SSE stream

        Raises:
            httpx.HTTPError: If the request fails
        """
        params: dict[str, str] = {"process": process}
        if duration is not None:
            params["duration"] = str(duration)

        transport = httpx.AsyncHTTPTransport(uds=str(self.socket_path))
        async with httpx.AsyncClient(transport=transport, timeout=None) as client:
            async with client.stream(
                "GET", f"{self.base_url}/logs", params=params
            ) as response:
                response.raise_for_status()

                skip_next_data = False

                async for line in response.aiter_lines():
                    if not line:
                        continue

                    # Check for sentinel event marking end of buffered logs
                    if line.startswith("event: buffered_done"):
                        skip_next_data = True
                        yield StreamEvent.BUFFERED_DONE
                        continue

                    # Parse SSE data lines
                    if line.startswith("data: "):
                        # Skip the data line that comes after buffered_done event
                        if skip_next_data:
                            skip_next_data = False
                            continue

                        data_str = line[6:]  # Remove "data: " prefix
                        try:
                            yield LogEntry.model_validate(json.loads(data_str))
                        except ValidationError:
                            # Skip malformed log entries
                            continue
