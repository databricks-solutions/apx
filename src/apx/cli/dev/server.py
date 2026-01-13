"""Centralized FastAPI dev server with reverse proxy for frontend and backend.

Architecture:
- Runs as a TCP server on localhost:9000-9999
- Reverse proxies requests:
  - `/__apx__/*` -> Internal management endpoints
  - `/<api_prefix>/*` -> Backend server (subprocess)
  - `/*` -> Frontend server (vite/bun subprocess)
- Manages both frontend and backend as subprocesses for clean shutdown
- Supports WebSocket proxying for HMR and real-time features

Key Features:
1. Single entry point for all development traffic
2. Reverse proxy with WebSocket support
3. In-memory log streaming via SSE
4. Graceful shutdown with SIGTERM to subprocesses
"""

import asyncio
import datetime
import json
import os
import signal
import subprocess
from collections import deque
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Literal

import time

import uvicorn
from fastapi import BackgroundTasks, FastAPI, Query, WebSocket
from fastapi.responses import Response, StreamingResponse
from starlette.requests import Request

from apx.cli.dev.logging import (
    DevLogComponent,
    LogBuffer,
    collect_subprocess_output,
    configure_dev_logging,
    get_logger,
    log_channel,
)
from apx.cli.dev.core import (
    is_port_available,
    find_listeners_for_port,
    kill_pids,
    kill_process_group,
    pids_belong_to_app,
    stop_tracked_process,
    track_process,
    wait_for_no_descendants,
    wait_for_port_free,
)
from apx.cli.dev.proxy import ProxyManager
from apx.constants import DEFAULT_HOST
from apx.models import (
    ActionRequest,
    ActionResponse,
    BrowserLogPayload,
    DevServerConfig,
    FrontendProcessState,
    LogChannel,
    LogEntry,
    OpenApiStatusResponse,
    PortsConfig,
    PortsResponse,
    ProcessRunningStatus,
    StatusResponse,
    TrackedProcess,
)

logger = get_logger(DevLogComponent.SERVER)


# Global state for background tasks
class ServerState(FrontendProcessState):
    """Global state for the dev server.

    Uses DevServerConfig as the single source of configuration.
    Both frontend and backend are managed as subprocesses.
    """

    def __init__(self) -> None:
        # Frontend subprocess (bun run dev)
        self.frontend_task: asyncio.Task[None] | None = None
        self.frontend_process: asyncio.subprocess.Process | None = None
        self.frontend_tracked: TrackedProcess | None = None
        self.frontend_exit_code: int | None = None
        # Backend subprocess (apx dev _run_backend)
        self.backend_task: asyncio.Task[None] | None = None
        self.backend_process: asyncio.subprocess.Process | None = None
        self.backend_tracked: TrackedProcess | None = None
        self.backend_exit_code: int | None = None
        # Shared state
        self.log_buffer: LogBuffer = deque(maxlen=10000)
        self.app_dir: Path | None = None
        self.config: DevServerConfig = DevServerConfig()
        self.proxy: ProxyManager | None = None
        # OpenAPI regeneration timestamps
        self.openapi_schema_last_updated: datetime.datetime | None = None
        self.api_ts_last_updated: datetime.datetime | None = None
        # Shutdown signaling for SSE connections
        self.shutdown_event: asyncio.Event | None = None

    # Convenience properties for backwards compatibility
    @property
    def dev_server_port(self) -> int:
        return self.config.dev_server_port

    @property
    def frontend_port(self) -> int:
        return self.config.frontend_port

    @property
    def backend_port(self) -> int:
        return self.config.backend_port

    @property
    def host(self) -> str:
        return self.config.host

    @property
    def api_prefix(self) -> str:
        return self.config.api_prefix

    @property
    def obo(self) -> bool:
        return self.config.obo

    @property
    def openapi_enabled(self) -> bool:
        return self.config.openapi

    @property
    def max_retries(self) -> int:
        return self.config.max_retries

    def update_config(self, config: DevServerConfig) -> None:
        """Update the server configuration."""
        self.config = config

    def update_ports(self, ports: PortsConfig) -> None:
        """Update only the ports in the configuration."""
        self.config = DevServerConfig(
            ports=ports,
            host=self.config.host,
            api_prefix=self.config.api_prefix,
            obo=self.config.obo,
            openapi=self.config.openapi,
            max_retries=self.config.max_retries,
            watch=self.config.watch,
        )


state = ServerState()


# === Proxy Management ===


def _create_proxy() -> ProxyManager:
    """Create a ProxyManager with current configuration."""
    frontend_url = f"http://{state.host}:{state.frontend_port}"
    backend_url = f"http://{state.host}:{state.backend_port}"
    return ProxyManager(
        frontend_url=frontend_url,
        backend_url=backend_url,
        api_prefix=state.api_prefix,
    )


# === Process Cleanup ===


async def stop_children(*, verify_ports: bool = True) -> list[str]:
    """Stop frontend/backend/openapi processes and ensure clean shutdown.

    Shutdown sequence:
    1. Stop WebSocket connections (handled by proxy.shutdown before this)
    2. Stop proxying HTTP requests (handled by proxy.shutdown before this)
    3. Stop frontend subprocess - with SIGTERM -> SIGKILL
    4. Stop backend subprocess - with SIGTERM -> SIGKILL
    5. Stop OpenAPI task
    """
    stopped: list[str] = []

    # Capture PGIDs before we start stopping
    frontend_pgid = state.frontend_tracked.pgid if state.frontend_tracked else None
    backend_pgid = state.backend_tracked.pgid if state.backend_tracked else None

    # === Step 1: Stop frontend task ===
    if state.frontend_task and not state.frontend_task.done():
        state.frontend_task.cancel()
        try:
            await state.frontend_task
        except asyncio.CancelledError:
            pass
        state.frontend_task = None

    # === Step 2: Kill frontend process tree/group ===
    if state.frontend_process is not None or frontend_pgid is not None:
        tp = state.frontend_tracked or (
            track_process(state.frontend_process.pid)
            if state.frontend_process is not None
            else None
        )

        if frontend_pgid is not None:
            logger.info(f"Killing frontend process group pgid={frontend_pgid}")
            kill_process_group(
                frontend_pgid,
                sigterm_timeout=1.0,
                sigkill_timeout=1.0,
            )

        if tp is not None:
            stop_tracked_process(
                tp, name="frontend", sigterm_timeout=1.0, sigkill_timeout=1.0
            )
            wait_for_no_descendants(tp, timeout=2.0, poll=0.1)

        state.frontend_process = None
        state.frontend_tracked = None
        stopped.append("frontend")

    # === Step 3: Safety net - kill orphaned frontend processes ===
    if state.frontend_port:
        await asyncio.sleep(0.2)
        orphan_pids = find_listeners_for_port(state.frontend_port)
        if orphan_pids:
            logger.warning(
                f"Found orphaned processes on frontend port {state.frontend_port}: {orphan_pids}"
            )
            if state.app_dir is not None:
                app_pids = pids_belong_to_app(
                    orphan_pids, app_dir=state.app_dir, expected_pgid=frontend_pgid
                )
                if app_pids:
                    kill_pids(app_pids, name="orphaned-frontend", sig=signal.SIGTERM)
                    await asyncio.sleep(0.5)
                    remaining = find_listeners_for_port(state.frontend_port)
                    if remaining:
                        kill_pids(
                            remaining, name="orphaned-frontend", sig=signal.SIGKILL
                        )
                        await asyncio.sleep(0.3)
            else:
                kill_pids(orphan_pids, name="orphaned-frontend", sig=signal.SIGKILL)
                await asyncio.sleep(0.3)

    # === Step 4: Stop backend task ===
    if state.backend_task and not state.backend_task.done():
        state.backend_task.cancel()
        try:
            await state.backend_task
        except asyncio.CancelledError:
            pass
        state.backend_task = None

    # === Step 5: Kill backend process tree/group ===
    if state.backend_process is not None or backend_pgid is not None:
        tp = state.backend_tracked or (
            track_process(state.backend_process.pid)
            if state.backend_process is not None
            else None
        )

        if backend_pgid is not None:
            logger.info(f"Killing backend process group pgid={backend_pgid}")
            kill_process_group(
                backend_pgid,
                sigterm_timeout=1.0,
                sigkill_timeout=1.0,
            )

        if tp is not None:
            stop_tracked_process(
                tp, name="backend", sigterm_timeout=1.0, sigkill_timeout=1.0
            )
            wait_for_no_descendants(tp, timeout=2.0, poll=0.1)

        state.backend_process = None
        state.backend_tracked = None
        stopped.append("backend")

    # === Step 6: Safety net - kill orphaned backend processes ===
    if state.backend_port:
        await asyncio.sleep(0.2)
        orphan_pids = find_listeners_for_port(state.backend_port)
        if orphan_pids:
            logger.warning(
                f"Found orphaned processes on backend port {state.backend_port}: {orphan_pids}"
            )
            kill_pids(orphan_pids, name="orphaned-backend", sig=signal.SIGTERM)
            await asyncio.sleep(0.5)
            remaining = find_listeners_for_port(state.backend_port)
            if remaining:
                kill_pids(remaining, name="orphaned-backend", sig=signal.SIGKILL)
                await asyncio.sleep(0.3)

    # === Step 7: Verify ports are free (optional) ===
    if verify_ports:
        for port_name, port in [
            ("frontend", state.frontend_port),
            ("backend", state.backend_port),
        ]:
            if not port:
                continue
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=port,
                timeout=2.0,
                poll=0.1,
            ):
                pids = find_listeners_for_port(port)
                if pids:
                    logger.error(f"{port_name} port {port} still in use: {pids}")
                    kill_pids(pids, name=f"{port_name}-port-hog", sig=signal.SIGKILL)
                    if not wait_for_port_free(
                        is_port_available_fn=is_port_available,
                        port=port,
                        timeout=1.0,
                        poll=0.1,
                    ):
                        pids2 = find_listeners_for_port(port)
                        raise RuntimeError(
                            f"{port_name} port {port} still in use (PIDs: {pids2})"
                        )

    return stopped


def request_dev_server_shutdown(delay: float = 0.0) -> None:
    """Terminate the dev server process (used by stop endpoint).

    Args:
        delay: Optional delay in seconds before sending SIGTERM.
               Used to allow HTTP response to be sent before shutdown.
    """
    if delay > 0:
        time.sleep(delay)
    os.kill(os.getpid(), signal.SIGTERM)


# Exit code 143 = SIGTERM (128 + 15)
SIGTERM_EXIT_CODE = 143


async def _check_auto_shutdown() -> None:
    """Check if both frontend and backend have exited with SIGTERM and trigger shutdown.

    When both managed processes exit with code 143 (SIGTERM), the dev server
    should automatically shut down since the user likely ran multiple stop commands.
    """
    # Both tasks must be done (completed or crashed)
    frontend_done = state.frontend_task is None or state.frontend_task.done()
    backend_done = state.backend_task is None or state.backend_task.done()

    if not (frontend_done and backend_done):
        return

    # Both have exited - check if both due to SIGTERM (143)
    both_sigterm = (
        state.frontend_exit_code == SIGTERM_EXIT_CODE
        and state.backend_exit_code == SIGTERM_EXIT_CODE
    )
    if both_sigterm:
        logger.info(
            "Both frontend and backend received SIGTERM (exit code 143), "
            "shutting down dev server"
        )
        # Give a brief moment for logs to flush
        await asyncio.sleep(0.5)
        request_dev_server_shutdown(delay=0.1)


# === Background Task Runners ===


async def run_frontend_task(
    app_dir: Path, port: int, max_retries: int, dev_server_port: int
) -> None:
    """Run frontend as a subprocess and capture logs."""
    ui_logger = get_logger(DevLogComponent.UI)
    ui_logger.info(f"Starting frontend server on port {port}")

    # Create process group/session for clean shutdown
    creationflags = 0
    start_new_session = False
    if os.name == "nt":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
    else:
        start_new_session = True

    # Pass port configuration via environment
    env = os.environ.copy()
    env["APX_FRONTEND_PORT"] = str(port)
    env["APX_DEV_SERVER_PORT"] = str(dev_server_port)

    try:
        process = await asyncio.create_subprocess_exec(
            "bun",
            "run",
            "dev",
            cwd=app_dir,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=start_new_session,
            creationflags=creationflags,
            env=env,
        )

        state.frontend_process = process
        state.frontend_tracked = track_process(process.pid)

        # Use centralized log collector
        await collect_subprocess_output(
            process,
            channel=LogChannel.UI,
            component=DevLogComponent.UI,
        )

        await process.wait()

        # Capture exit code for status tracking
        state.frontend_exit_code = process.returncode

        if process.returncode != 0:
            ui_logger.error(f"Frontend process exited with code {process.returncode}")

        # Check if we should auto-shutdown (both processes exited with SIGTERM)
        await _check_auto_shutdown()

    except asyncio.CancelledError:
        raise
    except Exception as e:
        ui_logger.error(f"Frontend task failed: {e}")
        if state.frontend_process:
            tp = track_process(state.frontend_process.pid)
            if tp is not None:
                stop_tracked_process(tp, name="frontend")


async def run_backend_task(
    app_dir: Path,
    host: str,
    port: int,
    obo: bool,
) -> None:
    """Run backend as a subprocess and capture logs."""
    backend_logger = get_logger(DevLogComponent.BACKEND)
    backend_logger.info(f"Starting backend server on {host}:{port}")

    # Create process group/session for clean shutdown
    creationflags = 0
    start_new_session = False
    if os.name == "nt":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
    else:
        start_new_session = True

    try:
        process = await asyncio.create_subprocess_exec(
            "uv",
            "run",
            "apx",
            "dev",
            "_run_backend",
            str(app_dir),
            str(port),
            host,
            str(obo).lower(),
            cwd=app_dir,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=start_new_session,
            creationflags=creationflags,
        )

        state.backend_process = process
        state.backend_tracked = track_process(process.pid)

        # Use centralized log collector
        await collect_subprocess_output(
            process,
            channel=LogChannel.APP,
            component=DevLogComponent.BACKEND,
        )

        await process.wait()

        # Capture exit code for status tracking
        state.backend_exit_code = process.returncode

        if process.returncode != 0:
            backend_logger.error(
                f"Backend process exited with code {process.returncode}"
            )

        # Check if we should auto-shutdown (both processes exited with SIGTERM)
        await _check_auto_shutdown()

    except asyncio.CancelledError:
        raise
    except Exception as e:
        backend_logger.error(f"Backend task failed: {e}")
        if state.backend_process:
            tp = track_process(state.backend_process.pid)
            if tp is not None:
                stop_tracked_process(tp, name="backend")


# === Lifecycle Management ===


@asynccontextmanager
async def lifespan(_: FastAPI) -> AsyncGenerator[None, None]:
    """Lifespan context manager for the FastAPI app."""
    # Configure logging to write to shared in-memory buffer
    configure_dev_logging(buffer=state.log_buffer)

    # Initialize shutdown event for SSE connections
    state.shutdown_event = asyncio.Event()

    try:
        yield
    finally:
        # Signal SSE connections to close before other cleanup
        if state.shutdown_event:
            state.shutdown_event.set()

        # Shutdown proxy first (graceful WebSocket close)
        if state.proxy:
            await state.proxy.shutdown(timeout=5.0)

        # Stop children
        await stop_children(verify_ports=False)


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
        description="Centralized development server with reverse proxy for APX projects",
        version="1.0.0",
        lifespan=lifespan,
    )

    state.app_dir = app_dir

    # === Management Routes (/__apx__/*) ===

    @app.get("/__apx__/")
    async def apx_root():
        """Root endpoint for APX management."""
        return {"message": "APX Dev Server", "status": "running"}

    @app.get("/__apx__/status", response_model=StatusResponse)
    async def get_status():
        """Get the status of all running processes."""
        running_status = ProcessRunningStatus(
            frontend_running=state.frontend_task is not None
            and not state.frontend_task.done(),
            backend_running=state.backend_task is not None
            and not state.backend_task.done(),
            openapi_running=False,  # OpenAPI regeneration is triggered on-demand, not a task
            frontend_exit_code=state.frontend_exit_code,
            backend_exit_code=state.backend_exit_code,
        )
        return StatusResponse.from_config(state.config, running_status)

    @app.get("/__apx__/ports", response_model=PortsResponse)
    async def get_ports():
        """Get the port configuration."""
        return PortsResponse.from_config(state.config)

    @app.post("/__apx__/browser-logs", status_code=204)
    async def receive_browser_logs(request: Request) -> Response:
        """Receive browser logs from the frontend dev tools.

        The UI may send logs using `navigator.sendBeacon`, which frequently arrives as a
        raw JSON string without an `application/json` content-type. Accept both:
        - JSON object body
        - JSON string body
        """
        try:
            body = await request.body()
            if not body:
                return Response(status_code=204)

            payload_obj: object
            try:
                # Fast path: proper JSON content-type or parseable JSON bytes
                payload_obj = json.loads(body)
            except Exception:
                # Fallback: decode to text and try again (covers odd encodings)
                payload_obj = json.loads(body.decode("utf-8", errors="ignore"))

            # If the client sent a JSON string (double-encoded), decode once more.
            if isinstance(payload_obj, str):
                payload_obj = json.loads(payload_obj)

            payload = BrowserLogPayload.model_validate(payload_obj)
        except Exception:
            # Never fail the dev server due to browser log ingestion.
            return Response(status_code=204)

        log_entry = LogEntry(
            timestamp=datetime.datetime.fromtimestamp(
                payload.timestamp / 1000
            ).strftime("%Y-%m-%d %H:%M:%S"),
            level=payload.level.upper(),
            channel=LogChannel.UI,
            component="browser",
            content=f"{payload.source} | {payload.message}"
            + (f"\n{payload.stack}" if payload.stack else ""),
        )
        state.log_buffer.append(log_entry)
        return Response(status_code=204)

    @app.post("/__apx__/actions/start", response_model=ActionResponse)
    async def start_servers(request: ActionRequest) -> ActionResponse:
        """Start all development servers."""
        # Check if already running
        if (
            state.frontend_task
            and not state.frontend_task.done()
            or state.backend_task
            and not state.backend_task.done()
        ):
            return ActionResponse(status="error", message="Servers are already running")

        # Store configuration from request
        state.update_config(request.to_config())

        # Reset exit codes from any previous session
        state.frontend_exit_code = None
        state.backend_exit_code = None

        # Create proxy manager
        state.proxy = _create_proxy()

        if not state.app_dir:
            return ActionResponse(status="error", message="App directory not set")

        config = state.config

        # Start frontend subprocess
        state.frontend_task = asyncio.create_task(
            run_frontend_task(
                state.app_dir,
                config.frontend_port,
                config.max_retries,
                config.dev_server_port,
            )
        )
        # Wait briefly for the bun process to be created and tracked
        for _ in range(20):
            if state.frontend_tracked is not None or state.frontend_process is not None:
                break
            await asyncio.sleep(0.05)
        if state.frontend_tracked is None and state.frontend_process is not None:
            state.frontend_tracked = track_process(state.frontend_process.pid)

        # Start backend subprocess
        state.backend_task = asyncio.create_task(
            run_backend_task(
                state.app_dir,
                config.host,
                config.backend_port,
                config.obo,
            )
        )
        # Wait briefly for the backend process to be created and tracked
        for _ in range(20):
            if state.backend_tracked is not None or state.backend_process is not None:
                break
            await asyncio.sleep(0.05)
        if state.backend_tracked is None and state.backend_process is not None:
            state.backend_tracked = track_process(state.backend_process.pid)

        return ActionResponse(status="success", message="Servers started successfully")

    @app.post("/__apx__/actions/stop", response_model=ActionResponse)
    async def stop_servers(background_tasks: BackgroundTasks) -> ActionResponse:
        """Stop all development servers with graceful shutdown."""
        try:
            # Signal SSE log streams to close before other cleanup
            if state.shutdown_event:
                state.shutdown_event.set()
            # Give SSE connections a moment to receive the shutdown event
            await asyncio.sleep(0.15)

            # Shutdown proxy first (close WebSocket connections)
            if state.proxy:
                await state.proxy.shutdown(timeout=5.0)
                state.proxy = None

            # Stop children
            stopped = await stop_children(verify_ports=False)
        except Exception as e:
            return ActionResponse(status="error", message=str(e))

        if not stopped:
            return ActionResponse(status="error", message="No servers were running")

        # Schedule shutdown AFTER the response is sent.
        # The delay ensures the HTTP response is fully transmitted before SIGTERM.
        background_tasks.add_task(request_dev_server_shutdown, delay=0.1)
        return ActionResponse(
            status="success",
            message=f"Stopped servers: {', '.join(stopped)}",
        )

    @app.post("/__apx__/actions/restart", response_model=ActionResponse)
    async def restart_servers() -> ActionResponse:
        """Restart all development servers."""
        # Shutdown proxy
        if state.proxy:
            await state.proxy.shutdown(timeout=5.0)
            state.proxy = None

        # Stop children
        try:
            await stop_children(verify_ports=True)
        except Exception as e:
            return ActionResponse(status="error", message=str(e))

        # Start with stored configuration
        request = ActionRequest.from_config(state.config)

        start_response = await start_servers(request)

        if start_response.status == "success":
            return ActionResponse(
                status="success", message="Servers restarted successfully"
            )
        else:
            return start_response

    @app.post("/__apx__/actions/refresh-openapi", response_model=ActionResponse)
    async def refresh_openapi() -> ActionResponse:
        """Trigger OpenAPI schema and api.ts client regeneration.

        Calls CLI command `apx openapi --force` to ensure fresh app load
        and forced client regeneration.
        """
        if state.app_dir is None:
            return ActionResponse(status="error", message="App directory not set")

        openapi_logger = get_logger(DevLogComponent.OPENAPI)

        try:
            # Run CLI command with --force to ensure full regeneration
            cmd = ["uv", "run", "apx", "openapi", "--force"]
            result = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=state.app_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            stdout, stderr = await result.communicate()

            if result.returncode == 0:
                state.openapi_schema_last_updated = datetime.datetime.now()
                state.api_ts_last_updated = datetime.datetime.now()
                openapi_logger.info("OpenAPI schema and api.ts refreshed successfully")
                return ActionResponse(
                    status="success",
                    message=f"OpenAPI schema and api.ts regenerated at {state.openapi_schema_last_updated}",
                )
            else:
                error_msg = stderr.decode("utf-8", errors="replace").strip()
                if not error_msg:
                    error_msg = stdout.decode("utf-8", errors="replace").strip()
                openapi_logger.error(f"Failed to refresh OpenAPI: {error_msg}")
                return ActionResponse(
                    status="error",
                    message=f"Failed to refresh OpenAPI: {error_msg or 'Unknown error'}",
                )
        except Exception as e:
            openapi_logger.error(f"Failed to refresh OpenAPI: {e}")
            return ActionResponse(
                status="error", message=f"Failed to refresh OpenAPI: {e}"
            )

    @app.get("/__apx__/openapi-status", response_model=OpenApiStatusResponse)
    async def get_openapi_status() -> OpenApiStatusResponse:
        """Get OpenAPI regeneration status and timestamps."""
        from apx.models import ProjectMetadata

        openapi_schema_path: str | None = None
        api_ts_path: str | None = None

        if state.app_dir is not None:
            openapi_json = state.app_dir / ".apx" / "openapi.json"
            if openapi_json.exists():
                openapi_schema_path = str(openapi_json)

            try:
                metadata = ProjectMetadata.read()
                api_ts = (
                    state.app_dir / "src" / metadata.app_slug / "ui" / "lib" / "api.ts"
                )
                if api_ts.exists():
                    api_ts_path = str(api_ts)
            except Exception:
                pass

        return OpenApiStatusResponse(
            openapi_schema_path=openapi_schema_path,
            api_ts_path=api_ts_path,
            openapi_schema_last_updated=state.openapi_schema_last_updated,
            api_ts_last_updated=state.api_ts_last_updated,
        )

    @app.get("/__apx__/logs/snapshot", response_model=list[LogEntry])
    async def get_logs_snapshot(
        duration: Annotated[
            int | None, Query(description="Show logs from last N seconds")
        ] = None,
        channel: Annotated[
            Literal["app", "ui", "apx", "all"] | None,
            Query(description="Filter by log channel"),
        ] = "all",
        component: Annotated[
            str | None,
            Query(description="Filter by component (e.g. backend, openapi, ui)"),
        ] = None,
        include_system: Annotated[
            bool, Query(description="Include system [apx] logs in results")
        ] = False,
        limit: Annotated[
            int, Query(description="Maximum number of log entries to return")
        ] = 500,
    ) -> list[LogEntry]:
        """Return a bounded snapshot of logs (non-streaming)."""

        def _matches(log: LogEntry) -> bool:
            # Channel gating with special handling for proxy logs.
            # Proxy logs are in [apx] channel but visible by default.
            if channel == "apx":
                # Explicitly requesting [apx] channel - show all system logs
                if log.channel.value != "apx":
                    return False
            elif channel == "app":
                if log.channel.value != "app":
                    return False
            elif channel == "ui":
                if log.channel.value != "ui":
                    return False
            else:
                # channel == "all" (default)
                # Show [app], [ui], and proxy logs from [apx]
                # Hide other [apx] logs unless include_system is set
                if log.channel.value == "apx":
                    if not include_system and log.component != "proxy":
                        return False

            if component is not None and log.component != component:
                return False

            return True

        cutoff_time: datetime.datetime | None = None
        if duration:
            cutoff_time = datetime.datetime.now() - datetime.timedelta(seconds=duration)

        capped = max(0, min(int(limit), 5000))
        result_rev: list[LogEntry] = []

        for log in reversed(state.log_buffer):
            if not _matches(log):
                continue

            if cutoff_time:
                try:
                    log_time = datetime.datetime.strptime(
                        log.timestamp, "%Y-%m-%d %H:%M:%S"
                    )
                    if log_time < cutoff_time:
                        continue
                except Exception:
                    pass

            result_rev.append(log)
            if capped and len(result_rev) >= capped:
                break

        return list(reversed(result_rev))

    @app.get("/__apx__/logs")
    async def stream_logs(
        duration: Annotated[
            int | None, Query(description="Show logs from last N seconds")
        ] = None,
        channel: Annotated[
            Literal["app", "ui", "apx", "all"] | None,
            Query(description="Filter by log channel"),
        ] = "all",
        component: Annotated[
            str | None,
            Query(description="Filter by component (e.g. backend, openapi, ui)"),
        ] = None,
        include_system: Annotated[
            bool, Query(description="Include system [apx] logs in results")
        ] = False,
    ) -> StreamingResponse:
        """Stream logs using Server-Sent Events (SSE)."""

        def _matches(log: LogEntry) -> bool:
            # Channel gating with special handling for proxy logs.
            # Proxy logs are in [apx] channel but visible by default.
            if channel == "apx":
                # Explicitly requesting [apx] channel - show all system logs
                if log.channel.value != "apx":
                    return False
            elif channel == "app":
                if log.channel.value != "app":
                    return False
            elif channel == "ui":
                if log.channel.value != "ui":
                    return False
            else:
                # channel == "all" (default)
                # Show [app], [ui], and proxy logs from [apx]
                # Hide other [apx] logs unless include_system is set
                if log.channel.value == "apx":
                    if not include_system and log.component != "proxy":
                        return False

            if component is not None and log.component != component:
                return False

            return True

        async def event_generator() -> AsyncGenerator[str, None]:
            """Generate SSE events for log streaming."""
            cutoff_time: datetime.datetime | None = None
            if duration:
                cutoff_time = datetime.datetime.now() - datetime.timedelta(
                    seconds=duration
                )

            # Send existing logs
            buffered_logs: list[LogEntry] = list(state.log_buffer)
            for log in buffered_logs:
                if not _matches(log):
                    continue

                if cutoff_time:
                    try:
                        log_time = datetime.datetime.strptime(
                            log.timestamp, "%Y-%m-%d %H:%M:%S"
                        )
                        if log_time < cutoff_time:
                            continue
                    except Exception:
                        pass

                yield f"data: {json.dumps(log.model_dump())}\n\n"

            # Send sentinel for end of buffered logs
            yield "event: buffered_done\ndata: {}\n\n"

            # Stream new logs
            last_index = len(state.log_buffer) - 1

            while True:
                # Check if server is shutting down
                if state.shutdown_event and state.shutdown_event.is_set():
                    # Send shutdown event to client before closing
                    yield "event: server_shutdown\ndata: {}\n\n"
                    return

                # Wait for either 100ms OR shutdown event (whichever comes first)
                if state.shutdown_event:
                    try:
                        await asyncio.wait_for(state.shutdown_event.wait(), timeout=0.1)
                        # If we get here, shutdown was triggered
                        yield "event: server_shutdown\ndata: {}\n\n"
                        return
                    except asyncio.TimeoutError:
                        # Normal timeout, continue with log checking
                        pass
                else:
                    await asyncio.sleep(0.1)

                current_index = len(state.log_buffer) - 1
                if current_index > last_index:
                    new_logs: list[LogEntry] = list(state.log_buffer)[last_index + 1 :]
                    for log in new_logs:
                        if not _matches(log):
                            continue
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

    # === Proxy Routes ===

    @app.websocket("/{path:path}")
    async def websocket_proxy(
        websocket: WebSocket,
        path: str,  # pyright: ignore[reportUnusedParameter]
    ) -> None:
        """Proxy WebSocket connections to frontend or backend."""
        if state.proxy is None:
            await websocket.close(code=1001, reason="Proxy not initialized")
            return
        await state.proxy.proxy_websocket(websocket)

    @app.api_route(
        "/{path:path}",
        methods=["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"],
    )
    async def http_proxy(
        request: Request,
        path: str,  # pyright: ignore[reportUnusedParameter]
    ) -> Response:
        """Proxy HTTP requests to frontend or backend."""
        # Don't proxy /__apx__/* routes - they're handled above
        if request.url.path.startswith("/__apx__"):
            return Response(status_code=404, content="Not found")

        if state.proxy is None:
            return Response(
                status_code=503,
                content="Proxy not initialized. Start servers first.",
                media_type="text/plain",
            )

        # Use streaming for SSE endpoints
        accept = request.headers.get("accept", "")
        if "text/event-stream" in accept:
            return await state.proxy.proxy_http_streaming(request)

        return await state.proxy.proxy_http(request)

    return app


def run_dev_server(
    app_dir: Path,
    port: int,
    host: str = DEFAULT_HOST,
):
    """Run the dev server on a TCP port.

    Args:
        app_dir: Application directory
        port: TCP port to listen on
        host: Host to bind to
    """
    # Change to app directory so ProjectMetadata.read() works correctly
    os.chdir(app_dir)

    # Initialize state with partial config (ports will be fully set by start request)
    state.update_config(
        DevServerConfig(
            ports=PortsConfig(dev_server_port=port),
            host=host,
        )
    )

    # Configure logging early so uvicorn startup logs are buffered and routed to [apx].
    configure_dev_logging(buffer=state.log_buffer)

    app = create_dev_server(app_dir)

    uvicorn_config = uvicorn.Config(
        app=app,
        host=host,
        port=port,
        log_level="info",
    )

    server = uvicorn.Server(uvicorn_config)
    with log_channel(LogChannel.APX):
        asyncio.run(server.serve())
