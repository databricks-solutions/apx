"""Centralized FastAPI dev server for managing frontend, backend, and OpenAPI watcher."""

import asyncio
from collections.abc import AsyncGenerator
import logging
import time
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Literal, TypeAlias
from typing_extensions import override

from fastapi import FastAPI, Query
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from apx.cli.dev.manager import (
    run_backend,
    run_frontend_with_logging,
    run_openapi_with_logging,
)
from apx.utils import ProjectMetadata, get_project_metadata


# === Log Entry Model ===


class LogEntry(BaseModel):
    """Strongly typed log entry model."""

    timestamp: str
    level: str
    process_name: str
    content: str


LogBuffer: TypeAlias = deque[LogEntry]


# Global state for background tasks
class ServerState:
    """Global state for the dev server."""

    def __init__(self) -> None:
        self.frontend_task: asyncio.Task[None] | None = None
        self.backend_task: asyncio.Task[None] | None = None
        self.openapi_task: asyncio.Task[None] | None = None
        self.log_buffer: LogBuffer = deque(maxlen=10000)
        self.app_dir: Path | None = None
        self.frontend_port: int = 5173
        self.backend_port: int = 8000
        self.backend_host: str = "0.0.0.0"
        self.obo: bool = True
        self.openapi_enabled: bool = True
        self.max_retries: int = 10


state = ServerState()


# === Pydantic Models ===


class ActionRequest(BaseModel):
    """Request model for action endpoints."""

    frontend_port: int = 5173
    backend_port: int = 8000
    backend_host: str = "0.0.0.0"
    obo: bool = True
    openapi: bool = True
    max_retries: int = 10


class ActionResponse(BaseModel):
    """Response model for action endpoints."""

    status: Literal["success", "error"]
    message: str


class StatusResponse(BaseModel):
    """Response model for status endpoint."""

    frontend_running: bool
    backend_running: bool
    openapi_running: bool
    frontend_port: int
    backend_port: int


# === Logging Setup ===


class BufferedLogHandler(logging.Handler):
    """Custom log handler that stores logs in memory buffer."""

    buffer: LogBuffer
    process_name: str

    def __init__(self, buffer: LogBuffer, process_name: str):
        super().__init__()
        self.buffer = buffer
        self.process_name = process_name

    @override
    def emit(self, record: logging.LogRecord) -> None:
        """Emit a log record to the buffer."""
        try:
            log_entry = LogEntry(
                timestamp=time.strftime(
                    "%Y-%m-%d %H:%M:%S", time.localtime(record.created)
                ),
                level=record.levelname,
                process_name=self.process_name,
                content=self.format(record),
            )
            self.buffer.append(log_entry)
        except Exception:
            self.handleError(record)


def setup_buffered_logging(process_name: str):
    """Setup logging that writes to in-memory buffer only.

    Args:
        process_name: Name of the process (frontend, backend, openapi)
    """
    logger = logging.getLogger(f"apx.{process_name}")
    logger.setLevel(logging.INFO)
    logger.handlers.clear()

    # Buffer handler (in-memory only)
    buffer_handler = BufferedLogHandler(state.log_buffer, process_name)
    buffer_formatter = logging.Formatter("%(message)s")
    buffer_handler.setFormatter(buffer_formatter)
    logger.addHandler(buffer_handler)

    logger.propagate = False


# === Background Task Runners ===


async def run_frontend_task(app_dir: Path, port: int, max_retries: int):
    """Run frontend as a background task."""
    try:
        await run_frontend_with_logging(app_dir, port, max_retries)
    except Exception as e:
        logger = logging.getLogger("apx.frontend")
        logger.error(f"Frontend task failed: {e}")


async def run_backend_task(
    app_dir: Path,
    app_module_name: str,
    host: str,
    port: int,
    obo: bool,
    log_file: Path | None,
    max_retries: int,
):
    """Run backend as a background task."""
    try:
        await run_backend(
            app_dir, app_module_name, host, port, obo, log_file, max_retries
        )
    except Exception as e:
        logger = logging.getLogger("apx.backend")
        logger.error(f"Backend task failed: {e}")


async def run_openapi_task(app_dir: Path, max_retries: int):
    """Run OpenAPI watcher as a background task."""
    try:
        await run_openapi_with_logging(app_dir, max_retries)
    except Exception as e:
        logger = logging.getLogger("apx.openapi")
        logger.error(f"OpenAPI watcher task failed: {e}")


# === Lifecycle Management ===


class LoggerWriter:
    """Logger writer for redirecting stdout/stderr to the backend logger."""

    def __init__(self, logger: logging.Logger, level: int, prefix: str):
        self.logger: logging.Logger = logger
        self.level: int = level
        self.prefix: str = prefix
        self.buffer: str = ""

    def write(self, message: str | None) -> None:
        if not message:
            return
        self.buffer += message
        lines = self.buffer.split("\n")
        self.buffer = lines[-1]
        for line in lines[:-1]:
            if line:
                self.logger.log(self.level, f"{self.prefix} | {line}")

    def flush(self):
        if self.buffer:
            self.logger.log(self.level, f"{self.prefix} | {self.buffer}")
            self.buffer = ""

    def isatty(self):
        return False


@asynccontextmanager
async def lifespan(_: FastAPI) -> AsyncGenerator[None, None]:
    """Lifespan context manager for the FastAPI app."""
    # Setup in-memory logging (no files needed)
    for process_name in ["frontend", "backend", "openapi"]:
        setup_buffered_logging(process_name)

    # Redirect stdout/stderr to backend logger BEFORE any tasks start
    # This ensures app imports capture logs correctly
    import sys

    original_stdout = sys.stdout
    original_stderr = sys.stderr

    backend_logger = logging.getLogger("apx.backend")

    sys.stdout = LoggerWriter(backend_logger, logging.INFO, "APP")
    sys.stderr = LoggerWriter(backend_logger, logging.ERROR, "APP")

    try:
        yield
    finally:
        # Restore stdout/stderr
        sys.stdout = original_stdout
        sys.stderr = original_stderr

    # Shutdown: Stop all tasks
    tasks_to_cancel: list[asyncio.Task[None]] = []
    if state.frontend_task and not state.frontend_task.done():
        tasks_to_cancel.append(state.frontend_task)
    if state.backend_task and not state.backend_task.done():
        tasks_to_cancel.append(state.backend_task)
    if state.openapi_task and not state.openapi_task.done():
        tasks_to_cancel.append(state.openapi_task)

    for task in tasks_to_cancel:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass


# === FastAPI App ===


def create_dev_server(app_dir: Path) -> FastAPI:
    """Create the dev server FastAPI app.

    Args:
        app_dir: Application directory

    Returns:
        FastAPI app instance
    """
    app = FastAPI(
        title="APX Dev Server",
        description="Centralized development server for APX projects",
        version="1.0.0",
        lifespan=lifespan,
    )

    state.app_dir = app_dir

    @app.get("/")
    async def root():
        """Root endpoint."""
        return {"message": "APX Dev Server", "status": "running"}

    @app.get("/status", response_model=StatusResponse)
    async def get_status():
        """Get the status of all running processes."""
        return StatusResponse(
            frontend_running=state.frontend_task is not None
            and not state.frontend_task.done(),
            backend_running=state.backend_task is not None
            and not state.backend_task.done(),
            openapi_running=state.openapi_task is not None
            and not state.openapi_task.done(),
            frontend_port=state.frontend_port,
            backend_port=state.backend_port,
        )

    @app.post("/actions/start", response_model=ActionResponse)
    async def start_servers(request: ActionRequest) -> ActionResponse:
        """Start all development servers."""
        # Check if already running
        if (
            state.frontend_task
            and not state.frontend_task.done()
            or state.backend_task
            and not state.backend_task.done()
            or state.openapi_task
            and not state.openapi_task.done()
        ):
            return ActionResponse(status="error", message="Servers are already running")

        # Store configuration
        state.frontend_port = request.frontend_port
        state.backend_port = request.backend_port
        state.backend_host = request.backend_host
        state.obo = request.obo
        state.openapi_enabled = request.openapi
        state.max_retries = request.max_retries

        # Get app module name
        if state.app_dir:
            metadata: ProjectMetadata = get_project_metadata()
            app_module_name: str = metadata.app_module
        else:
            return ActionResponse(status="error", message="App directory not set")

        # Start frontend
        if state.app_dir:
            state.frontend_task = asyncio.create_task(
                run_frontend_task(
                    state.app_dir,
                    request.frontend_port,
                    request.max_retries,
                )
            )

        # Start backend
        if state.app_dir:
            state.backend_task = asyncio.create_task(
                run_backend_task(
                    state.app_dir,
                    app_module_name,
                    request.backend_host,
                    request.backend_port,
                    request.obo,
                    None,  # log_file no longer used
                    request.max_retries,
                )
            )

        # Start OpenAPI watcher
        if request.openapi and state.app_dir:
            state.openapi_task = asyncio.create_task(
                run_openapi_task(state.app_dir, request.max_retries)
            )

        return ActionResponse(status="success", message="Servers started successfully")

    @app.post("/actions/stop", response_model=ActionResponse)
    async def stop_servers() -> ActionResponse:
        """Stop all development servers."""
        stopped: list[str] = []

        if state.frontend_task and not state.frontend_task.done():
            state.frontend_task.cancel()
            try:
                await state.frontend_task
            except asyncio.CancelledError:
                pass
            stopped.append("frontend")

        if state.backend_task and not state.backend_task.done():
            state.backend_task.cancel()
            try:
                await state.backend_task
            except asyncio.CancelledError:
                pass
            stopped.append("backend")

        if state.openapi_task and not state.openapi_task.done():
            state.openapi_task.cancel()
            try:
                await state.openapi_task
            except asyncio.CancelledError:
                pass
            stopped.append("openapi")

        if not stopped:
            return ActionResponse(status="error", message="No servers were running")

        return ActionResponse(
            status="success",
            message=f"Stopped servers: {', '.join(stopped)}",
        )

    @app.post("/actions/restart", response_model=ActionResponse)
    async def restart_servers() -> ActionResponse:
        """Restart all development servers."""
        # Stop first
        await stop_servers()

        # Wait a moment
        await asyncio.sleep(1)

        # Start with stored configuration
        request = ActionRequest(
            frontend_port=state.frontend_port,
            backend_port=state.backend_port,
            backend_host=state.backend_host,
            obo=state.obo,
            openapi=state.openapi_enabled,
            max_retries=state.max_retries,
        )

        return await start_servers(request)

    @app.get("/logs")
    async def stream_logs(
        duration: Annotated[
            int | None, Query(description="Show logs from last N seconds")
        ] = None,
        process: Annotated[
            Literal["frontend", "backend", "openapi", "all"] | None,
            Query(description="Filter by process name"),
        ] = "all",
    ) -> StreamingResponse:
        """Stream logs using Server-Sent Events (SSE)."""

        async def event_generator() -> AsyncGenerator[str, None]:
            """Generate SSE events for log streaming."""
            import datetime
            import json

            # Send initial buffered logs
            cutoff_time: datetime.datetime | None = None
            if duration:
                cutoff_time = datetime.datetime.now() - datetime.timedelta(
                    seconds=duration
                )

            # Send existing logs
            buffered_logs: list[LogEntry] = list(state.log_buffer)
            for log in buffered_logs:
                # Filter by process if specified
                if process != "all" and log.process_name != process:
                    continue

                # Filter by time if specified
                if cutoff_time:
                    try:
                        log_time = datetime.datetime.strptime(
                            log.timestamp, "%Y-%m-%d %H:%M:%S"
                        )
                        if log_time < cutoff_time:
                            continue
                    except Exception:
                        pass

                # Format SSE event (use model_dump for JSON serialization)
                yield f"data: {json.dumps(log.model_dump())}\n\n"

            # Send a sentinel event to mark end of buffered logs
            yield "event: buffered_done\ndata: {}\n\n"

            # Stream new logs as they arrive
            last_index = len(state.log_buffer) - 1

            while True:
                await asyncio.sleep(0.1)  # Check every 100ms

                # Check for new logs
                current_index = len(state.log_buffer) - 1
                if current_index > last_index:
                    # Get new logs
                    new_logs: list[LogEntry] = list(state.log_buffer)[last_index + 1 :]
                    for log in new_logs:
                        # Filter by process if specified
                        if process != "all" and log.process_name != process:
                            continue

                        # Format SSE event (use model_dump for JSON serialization)
                        yield f"data: {json.dumps(log.model_dump())}\n\n"

                    last_index = current_index

        return StreamingResponse(
            event_generator(),
            media_type="text/event-stream",
            headers={
                "Cache-Control": "no-cache",
                "Connection": "keep-alive",
                "X-Accel-Buffering": "no",
            },
        )

    return app


def run_dev_server(app_dir: Path, port: int = 8040, host: str = "127.0.0.1"):
    """Run the dev server.

    Args:
        app_dir: Application directory
        port: Port to run the server on
        host: Host to bind to
    """
    import os
    import uvicorn

    # Change to app directory so get_project_metadata() works correctly
    os.chdir(app_dir)

    app = create_dev_server(app_dir)

    config = uvicorn.Config(
        app=app,
        host=host,
        port=port,
        log_level="info",
    )

    server = uvicorn.Server(config)
    asyncio.run(server.serve())
