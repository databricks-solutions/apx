"""HTTP client for communicating with the dev server using httpx."""

import json
from collections.abc import AsyncIterator, Iterator
from contextlib import contextmanager
from enum import Enum
from typing import Literal

import httpx

from apx.cli.dev.models import ActionRequest, ActionResponse, LogEntry, StatusResponse


class StreamEvent(str, Enum):
    """Marker for special SSE events."""

    BUFFERED_DONE = "buffered_done"


class DevServerClient:
    """Client for communicating with the dev server."""

    def __init__(self, base_url: str, timeout: float = 5.0):
        """Initialize the dev server client.

        Args:
            base_url: Base URL of the dev server (e.g., "http://localhost:8040")
            timeout: Default timeout for requests in seconds
        """
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

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
                f"{self.base_url}/actions/start",
                json=request.model_dump(),
            )
            response.raise_for_status()
            return ActionResponse(**response.json())

    def stop(self) -> ActionResponse:
        """Stop the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=self.timeout) as client:
            response = client.post(f"{self.base_url}/actions/stop")
            response.raise_for_status()
            return ActionResponse(**response.json())

    def restart(self) -> ActionResponse:
        """Restart the development servers.

        Returns:
            ActionResponse indicating success or failure

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=10.0) as client:  # Longer timeout for restart
            response = client.post(f"{self.base_url}/actions/restart")
            response.raise_for_status()
            return ActionResponse(**response.json())

    def status(self) -> StatusResponse:
        """Get the status of development servers.

        Returns:
            StatusResponse with current server status

        Raises:
            httpx.HTTPError: If the request fails
        """
        with httpx.Client(timeout=self.timeout) as client:
            response = client.get(f"{self.base_url}/status")
            response.raise_for_status()
            return StatusResponse(**response.json())

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

        with httpx.Client(timeout=None) as client:
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
                                log_data = json.loads(data_str)
                                # Validate that we have all required fields before creating LogEntry
                                if all(
                                    key in log_data
                                    for key in [
                                        "timestamp",
                                        "level",
                                        "process_name",
                                        "content",
                                    ]
                                ):
                                    yield LogEntry(**log_data)
                            except (json.JSONDecodeError, TypeError, ValueError):
                                # Skip malformed log entries
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

        async with httpx.AsyncClient(timeout=None) as client:
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
                            log_data = json.loads(data_str)
                            # Validate that we have all required fields before creating LogEntry
                            if all(
                                key in log_data
                                for key in [
                                    "timestamp",
                                    "level",
                                    "process_name",
                                    "content",
                                ]
                            ):
                                yield LogEntry(**log_data)
                        except (json.JSONDecodeError, TypeError, ValueError):
                            # Skip malformed log entries
                            continue
