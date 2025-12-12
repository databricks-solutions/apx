"""Centralized FastAPI dev server for managing frontend, backend, and OpenAPI watcher.

Architecture:
- Runs as a Unix domain socket server
- Manages three background tasks: frontend (Node.js/Bun), backend (Python), OpenAPI watcher
- Uses process groups (start_new_session=True) to enable killing all child processes
- Explicit process cleanup on stop/restart to prevent orphaned processes
- Cleanup of orphaned processes on start for bulletproof operation

Key Features:
1. Process Group Killing: Uses os.killpg() to kill entire process trees
2. Graceful Shutdown: SIGTERM first, then SIGKILL after timeout
3. Orphan Cleanup: Scans for and kills orphaned bun/node/vite processes on start
4. State Tracking: Maintains references to subprocess objects for explicit cleanup
5. Port Cleanup: Kills processes holding onto ports before starting new servers
"""

import asyncio
from collections.abc import AsyncGenerator
import logging
import os
import signal

from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Literal

from fastapi import FastAPI, Query
from fastapi.responses import StreamingResponse

from apx.cli.dev.manager import (
    is_port_available,
    run_backend,
    run_frontend_with_logging,
    run_openapi_with_logging,
)
from apx.cli.dev.models import (
    ActionRequest,
    ActionResponse,
    LogEntry,
    PortsResponse,
    StatusResponse,
)
from apx.cli.dev.logging import (
    LogBuffer,
    LoggerWriter,
    setup_buffered_logging,
)
from apx.cli.dev.process_control import (
    TrackedProcess,
    find_listeners_for_port,
    kill_pids,
    pids_belong_to_app,
    stop_tracked_process,
    track_process,
    wait_for_no_descendants,
    wait_for_port_free,
)


# Global state for background tasks
class ServerState:
    """Global state for the dev server."""

    def __init__(self) -> None:
        from collections import deque

        self.frontend_task: asyncio.Task[None] | None = None
        self.backend_task: asyncio.Task[None] | None = None
        self.openapi_task: asyncio.Task[None] | None = None
        self.frontend_process: asyncio.subprocess.Process | None = None
        self.frontend_tracked: TrackedProcess | None = None
        self.log_buffer: LogBuffer = deque(maxlen=10000)
        self.app_dir: Path | None = None
        self.frontend_port: int = 5173
        self.backend_port: int = 8000
        self.host: str = "localhost"
        self.obo: bool = True
        self.openapi_enabled: bool = True
        self.max_retries: int = 10


state = ServerState()


# === Process Cleanup ===
# Process lifecycle is handled by `apx.cli.dev.process_control` and stored in `.apx/project.json`.


def _project_json_path() -> Path | None:
    if not state.app_dir:
        return None
    return state.app_dir / ".apx" / "project.json"


def _update_project_frontend_process(tp: TrackedProcess | None) -> None:
    """Persist frontend process metadata to `.apx/project.json` (best-effort)."""
    path = _project_json_path()
    if path is None:
        return
    from apx.cli.dev.manager import read_project_config, write_project_config
    from apx.cli.dev.models import DevProcessInfo, ProjectConfig

    try:
        config = read_project_config(path) if path.exists() else ProjectConfig()
    except Exception:
        config = ProjectConfig()

    config.dev.frontend_port = state.frontend_port
    config.dev.backend_port = state.backend_port
    config.dev.host = state.host
    if tp is None:
        config.dev.frontend_process = None
    else:
        config.dev.frontend_process = DevProcessInfo(
            pid=tp.pid,
            create_time=tp.create_time,
            pgid=tp.pgid,
        )
    write_project_config(path, config)


async def stop_children(*, verify_ports: bool = True) -> list[str]:
    """Stop frontend/backend/openapi tasks and ensure frontend process tree is gone."""
    stopped: list[str] = []

    # Frontend: cancel task, then stop tracked process tree/group.
    if state.frontend_task and not state.frontend_task.done():
        state.frontend_task.cancel()
        try:
            await state.frontend_task
        except asyncio.CancelledError:
            pass
        state.frontend_task = None

    if state.frontend_process is not None:
        tp = state.frontend_tracked or track_process(state.frontend_process.pid)
        if tp is not None:
            stop_tracked_process(tp, name="frontend")
            wait_for_no_descendants(tp, timeout=5.0, poll=0.1)
        state.frontend_process = None
        state.frontend_tracked = None
        stopped.append("frontend")

    if state.backend_task and not state.backend_task.done():
        state.backend_task.cancel()
        try:
            await state.backend_task
        except asyncio.CancelledError:
            pass
        state.backend_task = None
        stopped.append("backend")

    if state.openapi_task and not state.openapi_task.done():
        state.openapi_task.cancel()
        try:
            await state.openapi_task
        except asyncio.CancelledError:
            pass
        state.openapi_task = None
        stopped.append("openapi")

    # Persist cleared frontend pid.
    _update_project_frontend_process(None)

    if verify_ports:
        # Verify frontend+backend ports are free (best-effort; backend is in-process)
        if state.frontend_port:
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=state.frontend_port,
                timeout=2.0,
                poll=0.1,
            ):
                # Try to kill remaining listeners that belong to this app (fast, scoped).
                pids = find_listeners_for_port(state.frontend_port)
                if state.app_dir is not None and pids:
                    expected_pgid = (
                        state.frontend_tracked.pgid if state.frontend_tracked else None
                    )
                    to_kill = pids_belong_to_app(
                        pids, app_dir=state.app_dir, expected_pgid=expected_pgid
                    )
                    if to_kill:
                        kill_pids(to_kill, name="frontend-listener", sig=signal.SIGKILL)
                if not wait_for_port_free(
                    is_port_available_fn=is_port_available,
                    port=state.frontend_port,
                    timeout=1.5,
                    poll=0.1,
                ):
                    pids2 = find_listeners_for_port(state.frontend_port)
                    raise RuntimeError(
                        f"Frontend port {state.frontend_port} still in use (listening PIDs: {pids2})"
                    )
        if state.backend_port:
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=state.backend_port,
                timeout=8.0,
                poll=0.1,
            ):
                pids = find_listeners_for_port(state.backend_port)
                raise RuntimeError(
                    f"Backend port {state.backend_port} still in use (listening PIDs: {pids})"
                )

    return stopped


def request_dev_server_shutdown() -> None:
    """Terminate the dev server process (used by /actions/stop only)."""
    os.kill(os.getpid(), signal.SIGTERM)


# === Background Task Runners ===


async def run_frontend_task(app_dir: Path, port: int, max_retries: int):
    """Run frontend as a background task."""
    try:
        await run_frontend_with_logging(app_dir, port, max_retries, state)
    except asyncio.CancelledError:
        # Frontend process cleanup is handled by stop_children()
        raise
    except Exception as e:
        logger = logging.getLogger("apx.frontend")
        logger.error(f"Frontend task failed: {e}")
        if state.frontend_process:
            tp = track_process(state.frontend_process.pid)
            if tp is not None:
                stop_tracked_process(tp, name="frontend")


async def run_backend_task(
    app_dir: Path,
    app_module_name: str,
    host: str,
    port: int,
    obo: bool,
    max_retries: int,
):
    """Run backend as a background task."""
    try:
        await run_backend(
            app_dir, app_module_name, host, port, obo, max_retries=max_retries
        )
    except asyncio.CancelledError:
        raise
    except Exception as e:
        logger = logging.getLogger("apx.backend")
        logger.error(f"Backend task failed: {e}")


async def run_openapi_task(app_dir: Path, max_retries: int):
    """Run OpenAPI watcher as a background task."""
    try:
        await run_openapi_with_logging(app_dir, max_retries)
    except asyncio.CancelledError:
        raise
    except Exception as e:
        logger = logging.getLogger("apx.openapi")
        logger.error(f"OpenAPI watcher task failed: {e}")


# === Lifecycle Management ===


@asynccontextmanager
async def lifespan(_: FastAPI) -> AsyncGenerator[None, None]:
    """Lifespan context manager for the FastAPI app."""
    # Setup in-memory logging (no files needed)
    for process_name in ["frontend", "backend", "openapi"]:
        setup_buffered_logging(state.log_buffer, process_name)

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

        # Stop children deterministically; do not kill-by-name unrelated processes.
        await stop_children(verify_ports=False)

        # Ensure project.json doesn't keep stale frontend process metadata.
        _update_project_frontend_process(None)


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

    @app.get("/ports", response_model=PortsResponse)
    async def get_ports():
        """Get the frontend and backend ports."""
        return PortsResponse(
            frontend_port=state.frontend_port,
            backend_port=state.backend_port,
            host=state.host,
        )

    @app.post("/actions/start", response_model=ActionResponse)
    async def start_servers(request: ActionRequest) -> ActionResponse:
        """Start all development servers.

        Ensures cleanup before starting to prevent duplicate frontend servers.
        """
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

        # If project.json has stale frontend process metadata, attempt pgid-based cleanup.
        if state.app_dir:
            from apx.cli.dev.manager import read_project_config

            path = _project_json_path()
            if path and path.exists():
                try:
                    cfg = read_project_config(path)
                    info = cfg.dev.frontend_process
                    if info is not None:
                        tp = TrackedProcess(
                            pid=info.pid, create_time=info.create_time, pgid=info.pgid
                        )
                        stop_tracked_process(tp, name="frontend")
                        wait_for_no_descendants(tp, timeout=5.0, poll=0.1)
                except Exception:
                    pass

        # Store configuration
        state.frontend_port = request.frontend_port
        state.backend_port = request.backend_port
        state.host = request.host
        state.obo = request.obo
        state.openapi_enabled = request.openapi
        state.max_retries = request.max_retries

        # Get app module name
        if state.app_dir:
            from apx.utils import get_project_metadata, ProjectMetadata

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
            # Wait briefly for the bun process to be created and tracked, then persist.
            for _ in range(20):
                if (
                    state.frontend_tracked is not None
                    or state.frontend_process is not None
                ):
                    break
                await asyncio.sleep(0.05)
            if state.frontend_tracked is None and state.frontend_process is not None:
                state.frontend_tracked = track_process(state.frontend_process.pid)
            if state.frontend_tracked is not None:
                _update_project_frontend_process(state.frontend_tracked)

        # Start backend
        if state.app_dir:
            state.backend_task = asyncio.create_task(
                run_backend_task(
                    state.app_dir,
                    app_module_name,
                    request.host,
                    request.backend_port,
                    request.obo,
                    request.max_retries,
                )
            )
            # Backend is in-process; nothing to persist beyond ports/host.

        # Start OpenAPI watcher
        if request.openapi and state.app_dir:
            state.openapi_task = asyncio.create_task(
                run_openapi_task(state.app_dir, request.max_retries)
            )
            # No additional process metadata to persist.

        return ActionResponse(status="success", message="Servers started successfully")

    @app.post("/actions/stop", response_model=ActionResponse)
    async def stop_servers() -> ActionResponse:
        """Stop all development servers.

        Explicitly kills all vite, bun, node, esbuild processes to ensure clean shutdown.
        """
        try:
            # For "stop", we terminate the dev-server process immediately after stopping
            # children. Port verification can be flaky on some platforms and is redundant
            # because the dev-server process (which owns the backend listener) is about to exit.
            stopped = await stop_children(verify_ports=False)
        except Exception as e:
            return ActionResponse(status="error", message=str(e))
        if not stopped:
            return ActionResponse(status="error", message="No servers were running")
        request_dev_server_shutdown()
        return ActionResponse(
            status="success",
            message=f"Stopped servers: {', '.join(stopped)}",
        )

    @app.post("/actions/restart", response_model=ActionResponse)
    async def restart_servers() -> ActionResponse:
        """Restart all development servers."""
        # Stop children only (do NOT terminate dev server process).
        try:
            await stop_children(verify_ports=True)
        except Exception as e:
            return ActionResponse(status="error", message=str(e))

        # Start with stored configuration
        request = ActionRequest(
            frontend_port=state.frontend_port,
            backend_port=state.backend_port,
            host=state.host,
            obo=state.obo,
            openapi=state.openapi_enabled,
            max_retries=state.max_retries,
        )

        start_response = await start_servers(request)

        # Combine messages
        if start_response.status == "success":
            return ActionResponse(
                status="success", message="Servers restarted successfully"
            )
        else:
            return start_response

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


def run_dev_server(app_dir: Path, socket_path: Path | str):
    """Run the dev server on a Unix domain socket.

    Args:
        app_dir: Application directory
        socket_path: Path to Unix domain socket
    """
    import os
    import uvicorn

    # Change to app directory so get_project_metadata() works correctly
    os.chdir(app_dir)

    # Ensure socket path is a Path object
    if isinstance(socket_path, str):
        socket_path = Path(socket_path)

    # Remove old socket file if it exists
    if socket_path.exists():
        socket_path.unlink()

    # Ensure parent directory exists
    socket_path.parent.mkdir(parents=True, exist_ok=True)

    app = create_dev_server(app_dir)

    config = uvicorn.Config(
        app=app,
        uds=str(socket_path),
        log_level="info",
    )

    server = uvicorn.Server(config)
    asyncio.run(server.serve())
