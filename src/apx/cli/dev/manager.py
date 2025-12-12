"""Development server utilities for apx."""

import asyncio
import json
import logging
import psutil
import socket
import subprocess
import time
import os
import signal
from pathlib import Path
from typing import Any, Literal

import keyring
from databricks.sdk import WorkspaceClient
from dotenv import load_dotenv
from fastapi import FastAPI, Request
from rich.table import Table
from starlette.middleware.base import BaseHTTPMiddleware
from tenacity import (
    RetryCallState,
    retry,
    retry_if_not_exception_type,
    stop_after_attempt,
    wait_exponential,
)
from typer import Exit
import watchfiles
import uvicorn
from apx.cli.dev.models import ActionRequest, LogEntry, ProjectConfig
from apx.cli.dev.client import DevServerClient, StreamEvent
from apx.cli.dev.logging import (
    setup_uvicorn_logging,
    suppress_output_and_logs,
    print_log_entry,
)
from apx.cli.dev.reloader import load_app as _load_app_from_reloader
from apx.utils import (
    console,
    ensure_dir,
)
from apx import __version__
from apx.cli.dev.process_control import (
    TrackedProcess,
    cleanup_dev_server_processes,
    find_listeners_for_port,
    kill_pids,
    pids_belong_to_app,
    stop_tracked_process,
    track_process,
    wait_for_no_descendants,
    wait_for_port_free,
)
from apx.cli.dev.models import DevProcessInfo
from apx.cli.dev.state_types import FrontendProcessState


# note: header name must be lowercase and with - symbols
ACCESS_TOKEN_HEADER_NAME = "x-forwarded-access-token"
FORWARDED_USER_HEADER_NAME = "x-forwarded-user"


# === Port Finding Utilities ===


def is_port_available(port: int, host: str = "127.0.0.1") -> bool:
    """Check if a port is available for binding.

    Uses multiple strategies to detect if a port is in use:
    1. Try connecting to the port (detects listening servers) on both IPv4 and IPv6
    2. Try binding to the port on both IPv4 and IPv6 addresses
    3. Check netstat-style connection listing for both IPv4 and IPv6

    Args:
        port: Port number to check
        host: Host to check on (default: 127.0.0.1)

    Returns:
        True if port is available, False otherwise
    """
    # Strategy 1: Try to connect on both IPv4 and IPv6
    # If we can connect, something is definitely listening

    # Check IPv4 (127.0.0.1)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.2)
            result = sock.connect_ex(("127.0.0.1", port))
            if result == 0:
                return False
    except (socket.error, OSError):
        pass

    # Check IPv6 (::1)
    try:
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.2)
            result = sock.connect_ex(("::1", port))
            if result == 0:
                return False
    except (socket.error, OSError):
        pass

    # Strategy 2: Try binding to both IPv4 and IPv6 addresses
    # A server can bind to either, so we must check both

    # Check IPv4: Try binding to BOTH 127.0.0.1 and 0.0.0.0
    for bind_host in ["127.0.0.1", "0.0.0.0"]:
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                # Don't set SO_REUSEADDR - we want to know if it's actually in use
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
                # Set SO_REUSEPORT to 0 as well on systems that support it
                if hasattr(socket, "SO_REUSEPORT"):
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 0)
                sock.bind((bind_host, port))
                # Bind succeeded, close immediately
        except OSError as e:
            # Bind failed - port is in use
            # errno 48 (macOS) or 98 (Linux) = Address already in use
            # errno 13 = Permission denied (might be privileged port)
            if e.errno in (48, 98, 13):
                return False
            # Other errors - also consider port unavailable to be safe
            return False

    # Check IPv6: Try binding to ::1 and ::
    for bind_host in ["::1", "::"]:
        try:
            with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
                # Don't set SO_REUSEADDR - we want to know if it's actually in use
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
                # Set SO_REUSEPORT to 0 as well on systems that support it
                if hasattr(socket, "SO_REUSEPORT"):
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 0)
                sock.bind((bind_host, port))
                # Bind succeeded, close immediately
        except OSError as e:
            # Bind failed - port is in use
            # errno 48 (macOS) or 98 (Linux) = Address already in use
            # errno 13 = Permission denied (might be privileged port)
            if e.errno in (48, 98, 13):
                return False
            # Other errors - also consider port unavailable to be safe
            return False

    # Strategy 3: Check system network connections as a fallback
    # This catches servers that might be in TIME_WAIT or other states
    # Check both IPv4 and IPv6 connections
    try:
        for conn in psutil.net_connections(kind="inet"):
            if hasattr(conn, "laddr") and conn.laddr and hasattr(conn.laddr, "port"):
                if conn.laddr.port == port:
                    # Port is in use by some process
                    return False
    except (psutil.AccessDenied, PermissionError, AttributeError):
        # If we can't check, be conservative and rely on bind test results
        pass

    try:
        for conn in psutil.net_connections(kind="inet6"):
            if hasattr(conn, "laddr") and conn.laddr and hasattr(conn.laddr, "port"):
                if conn.laddr.port == port:
                    # Port is in use by some process
                    return False
    except (psutil.AccessDenied, PermissionError, AttributeError):
        # If we can't check, be conservative and rely on bind test results
        pass

    return True


def find_available_port(start: int, end: int, host: str = "127.0.0.1") -> int | None:
    """Find an available port in the given range.

    Args:
        start: Start of port range (inclusive)
        end: End of port range (inclusive)
        host: Host to check on (default: 127.0.0.1)

    Returns:
        Available port number or None if no port is available
    """
    for port in range(start, end + 1):
        if is_port_available(port, host):
            return port
    return None


# === Retry Helpers ===


def log_retry_attempt(retry_state: RetryCallState) -> None:
    """Log retry attempts to the appropriate logger.

    Args:
        retry_state: Tenacity retry state
    """
    attempt_number = retry_state.attempt_number
    if retry_state.outcome and retry_state.outcome.failed:
        exception = retry_state.outcome.exception()
        logger = logging.getLogger("apx.retry")
        logger.error(
            f"Attempt {attempt_number} failed with error: {exception}. Retrying..."
        )


# === Project Configuration Utilities ===


def read_project_config(file_path: Path) -> ProjectConfig:
    """Read project config from file.

    Args:
        file_path: Path to project.json

    Returns:
        ProjectConfig instance
    """
    if not file_path.exists():
        raise FileNotFoundError(f"Project config not found at {file_path}")

    data: dict[str, Any] = json.loads(  # pyright: ignore[reportExplicitAny]
        file_path.read_text()
    )

    # Migrate old config structure to new structure
    if "dev_server_pid" in data or "dev_server_port" in data or "token_id" in data:
        # Old structure detected, migrate to new structure
        migrated_data = {
            "dev": {
                "token_id": data.get("token_id"),
                "pid": data.get("dev_server_pid"),
                "port": data.get("dev_server_port"),
            },
        }
        return ProjectConfig.model_validate(migrated_data)

    return ProjectConfig.model_validate(data)


def write_project_config(file_path: Path, config: ProjectConfig) -> None:
    """Write project config to file.

    Args:
        file_path: Path to project.json
        config: ProjectConfig instance to write
    """
    ensure_dir(file_path.parent)
    file_path.write_text(config.model_dump_json(indent=2))


def load_app(app_module_name: str, reload_modules: bool = False) -> FastAPI:
    """Load and return the FastAPI app instance.

    This function now uses the centralized reloader to prevent duplicate imports.
    """
    app_instance, _ = _load_app_from_reloader(app_module_name, reload=reload_modules)
    return app_instance


def create_obo_token(
    ws: WorkspaceClient,
    app_module_name: str,
    token_lifetime_seconds: int,
    status_context=None,
):
    """Create a new OBO token via Databricks API.

    Args:
        ws: WorkspaceClient instance
        app_module_name: Name of the app module
        token_lifetime_seconds: Token lifetime in seconds
        status_context: Optional status context for updates

    Returns:
        Tuple of (token_id, token_value)
    """
    if status_context:
        status_context.update("🔐 Creating new OBO token")

    # Suppress any logging during token creation
    with suppress_output_and_logs():
        new_token = ws.tokens.create(
            comment=f"dev token for {app_module_name}, created by apx",
            lifetime_seconds=token_lifetime_seconds,
        )

    assert new_token.token_info is not None
    assert new_token.token_info.token_id is not None
    assert new_token.token_value is not None

    if status_context:
        status_context.update("✅ Token created successfully")

    return new_token.token_info.token_id, new_token.token_value


def validate_databricks_credentials(ws: WorkspaceClient) -> bool:
    """Validate that Databricks credentials are valid and not expired.

    Args:
        ws: WorkspaceClient instance

    Returns:
        True if credentials are valid, False otherwise
    """
    try:
        with suppress_output_and_logs():
            # Try to get current user info - simple API call to validate credentials
            ws.current_user.me()
        return True
    except Exception as e:
        error_str = str(e).lower()
        # Check for common authentication errors
        if (
            "invalid" in error_str
            or "token" in error_str
            or "401" in error_str
            or "403" in error_str
        ):
            return False
        # For other errors, assume credentials are valid but something else is wrong
        raise


def prepare_obo_token(
    cwd: Path,
    app_module_name: str,
    token_lifetime_seconds: int = 60 * 60 * 4,
    status_context=None,
) -> str:
    """Prepare the On-Behalf-Of token for the backend server.

    Checks keyring and project.json for existing valid token, creates new one if needed.
    Only stores in keyring (secure) and token_id in project.json (not sensitive).
    """
    # Initialize Databricks client (credentials should already be validated by this point)
    try:
        with suppress_output_and_logs():
            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    except Exception as e:
        console.print(f"[red]❌ Failed to initialize Databricks client: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
        )
        raise Exit(code=1)

    # Use project directory path as keyring identifier
    keyring_id = str(cwd.resolve())

    # Step 1: Check keyring for token
    if status_context:
        status_context.update("🔍 Checking keyring for existing token")

    keyring_token = get_token_from_keyring(keyring_id)
    stored_token_id = get_token_id(cwd)

    # If we have both token and token_id, validate the token
    if keyring_token and stored_token_id:
        if status_context:
            status_context.update("🔐 Validating existing token")

        # Suppress any logging during token validation
        with suppress_output_and_logs():
            user_tokens = ws.tokens.list()
            user_token = next(
                (token for token in user_tokens if token.token_id == stored_token_id),
                None,
            )

        # Check if token exists and is still valid
        if user_token and user_token.expiry_time:
            expiry_timestamp = user_token.expiry_time / 1000
            current_time = time.time()
            time_remaining = expiry_timestamp - current_time

            # Use existing token if it has at least 1 hour remaining
            min_remaining_time = 60 * 60
            if time_remaining > min_remaining_time:
                if status_context:
                    status_context.update(
                        f"✅ Using existing token (expires in {int(time_remaining / 3600)} hours)"
                    )
                return keyring_token
            else:
                if status_context:
                    status_context.update("⚠️  Token expiring soon, rotating...")
        else:
            if status_context:
                status_context.update("⚠️  Token invalid, creating new one...")
    elif keyring_token:
        # Have token but no token_id - clean up and recreate
        if status_context:
            status_context.update("⚠️  Token found but missing metadata, recreating...")
        delete_token_from_keyring(keyring_id)

    # Step 2: Create new token
    if status_context:
        status_context.update("🔐 Creating new OBO token")

    token_id, new_token = create_obo_token(
        ws,
        app_module_name,
        token_lifetime_seconds,
        status_context=status_context,
    )

    # Step 3: Store in keyring and project.json
    save_token_to_keyring(keyring_id, new_token)
    save_token_id(cwd, token_id)
    if status_context:
        status_context.update("💾 Token stored securely in keyring")

    return new_token


async def run_backend(
    cwd: Path,
    app_module_name: str,
    host: str,
    backend_port: int,
    obo: bool = False,
    log_file: Path | None = None,
    max_retries: int = 10,
):
    """Run the backend server programmatically with uvicorn and hot-reload support.

    Args:
        cwd: Current working directory
        app_module_name: Module name for the FastAPI app
        host: Host to bind to
        backend_port: Port to bind to
        obo: Whether to enable On-Behalf-Of token middleware
        log_file: Deprecated, kept for compatibility (use None)
        max_retries: Maximum number of retry attempts
    """

    # Setup uvicorn logging once at the start
    # If log_file is None, we're in dev_server mode and use memory logging
    use_memory = log_file is None
    setup_uvicorn_logging(use_memory=use_memory)

    # Setup retry logger
    retry_logger = logging.getLogger("apx.retry")
    retry_logger.setLevel(logging.INFO)
    retry_logger.handlers.clear()

    if use_memory:
        # Use the backend logger that's already configured
        backend_logger = logging.getLogger("apx.backend")
        if backend_logger.handlers:
            retry_logger.addHandler(backend_logger.handlers[0])
    else:
        # Console mode - use uvicorn handler
        uvicorn_logger = logging.getLogger("uvicorn")
        if uvicorn_logger.handlers:
            retry_logger.addHandler(uvicorn_logger.handlers[0])

    retry_logger.propagate = False

    # Note: stdout/stderr redirection is handled in dev_server.py lifespan
    # before any tasks start, so we don't need to do it here.

    @retry(
        stop=stop_after_attempt(max_retries),
        wait=wait_exponential(multiplier=1, min=2, max=60),
        before_sleep=log_retry_attempt,
        reraise=True,
    )
    async def run_backend_with_retry():
        """Backend runner with retry logic."""
        backend_logger = logging.getLogger("uvicorn")

        if use_memory:
            backend_logger.info(f"Starting backend server on {host}:{backend_port}")
        else:
            console.print(
                f"[green][server][/]Starting server on {host}:{backend_port} from app: {app_module_name}"
            )
            console.print(f"[green][server][/]Watching for changes in {cwd}/**/*.py")
            console.print()

        # Track if this is the first run
        first_run = True

        # Store OBO token for reuse
        obo_token = None

        while True:
            server = None
            server_task = None
            watch_task = None

            try:
                # Reload message
                if not first_run and not use_memory:
                    console.print("[yellow][server][/yellow] Reloading...")
                    console.print()

                # Reload .env file on every iteration (including first run)
                dotenv_file = cwd / ".env"
                if dotenv_file.exists():
                    # Override=True ensures we reload env vars on hot reload
                    load_dotenv(dotenv_file)

                # Prepare OBO token (will reuse if still valid)
                if obo and first_run:
                    if use_memory:
                        obo_token = prepare_obo_token(
                            cwd, app_module_name, status_context=None
                        )
                    else:
                        with console.status(
                            "[bold cyan]Preparing On-Behalf-Of token..."
                        ) as status:
                            status.update(
                                f"📂 Loading .env file from {dotenv_file.resolve()}"
                            )
                            obo_token = prepare_obo_token(
                                cwd, app_module_name, status_context=status
                            )
                            # Give user a moment to see the final status
                            time.sleep(0.3)
                        console.print("[green]✓[/green] On-Behalf-Of token ready")
                        console.print()
                elif obo:
                    # On hot reload, prepare token without spinner
                    obo_token = prepare_obo_token(
                        cwd, app_module_name, status_context=None
                    )

                # Load/reload the app instance (fully reload modules on hot reload)
                app_instance = load_app(app_module_name, reload_modules=not first_run)

                ws = WorkspaceClient(product="apx/dev", product_version=__version__)
                user_id = ws.current_user.me().id
                assert user_id is not None, "User ID is not set"
                try:
                    # heuristic to get workspace id from host
                    workspace_id = ws.config.host.split("-")[1].split(".")[0]
                except Exception:
                    workspace_id = "placeholder"

                async def user_id_middleware(request: Request, call_next):
                    user_id_header: tuple[bytes, bytes] = (
                        FORWARDED_USER_HEADER_NAME.encode(),
                        f"{user_id}@{workspace_id}".encode(),
                    )
                    request.headers.__dict__["_list"].append(user_id_header)
                    return await call_next(request)

                app_instance.add_middleware(
                    BaseHTTPMiddleware, dispatch=user_id_middleware
                )

                # Add OBO middleware if enabled
                if obo and obo_token:
                    assert obo_token is not None, "OBO token is not set"
                    encoded_token = obo_token.encode()

                    async def obo_middleware(request: Request, call_next):
                        # Headers are immutable, so we need to append to the list
                        token_header: tuple[bytes, bytes] = (
                            ACCESS_TOKEN_HEADER_NAME.encode(),
                            encoded_token,
                        )
                        request.headers.__dict__["_list"].append(token_header)
                        return await call_next(request)

                    app_instance.add_middleware(
                        BaseHTTPMiddleware, dispatch=obo_middleware
                    )

                if first_run:
                    console.print()

                config = uvicorn.Config(
                    app=app_instance,
                    host=host,
                    port=backend_port,
                    log_level="info",
                    log_config=None,  # Disable uvicorn's default log config
                )

                server = uvicorn.Server(config)
                first_run = False

                # Start server in a background task
                async def serve(server_instance: uvicorn.Server):
                    try:
                        await server_instance.serve()
                    except asyncio.CancelledError:
                        pass

                server_task = asyncio.create_task(serve(server))

                # Watch for file changes
                async def watch_files():
                    async for changes in watchfiles.awatch(
                        cwd,
                        watch_filter=watchfiles.PythonFilter(),
                    ):
                        if not use_memory:
                            console.print(
                                f"[yellow][server][/yellow] Detected changes in {len(changes)} file(s)"
                            )
                        return

                watch_task = asyncio.create_task(watch_files())

                # Wait for either server to crash or files to change
                done, pending = await asyncio.wait(
                    [server_task, watch_task],
                    return_when=asyncio.FIRST_COMPLETED,
                )

                # Shutdown server gracefully
                if server:
                    server.should_exit = True
                    # Give it a moment to shut down
                    await asyncio.sleep(0.5)

                # Cancel remaining tasks
                for task in pending:
                    task.cancel()
                    try:
                        await task
                    except asyncio.CancelledError:
                        pass

                # If server task completed (crashed), re-raise the exception
                if server_task in done:
                    exc = server_task.exception()
                    if exc:
                        raise exc

            except asyncio.CancelledError:
                # IMPORTANT: allow task cancellation to actually stop the backend.
                # Without this, CancelledError would be caught by the generic Exception
                # handler below and the loop would keep running.
                if server:
                    server.should_exit = True
                if server_task and not server_task.done():
                    server_task.cancel()
                    try:
                        await server_task
                    except asyncio.CancelledError:
                        pass
                if watch_task and not watch_task.done():
                    watch_task.cancel()
                    try:
                        await watch_task
                    except asyncio.CancelledError:
                        pass
                raise
            except KeyboardInterrupt:
                # Clean shutdown on Ctrl+C
                if server:
                    server.should_exit = True

                if server_task and not server_task.done():
                    server_task.cancel()
                    try:
                        await server_task
                    except asyncio.CancelledError:
                        pass

                if watch_task and not watch_task.done():
                    watch_task.cancel()
                    try:
                        await watch_task
                    except asyncio.CancelledError:
                        pass

                raise
            except Exception as e:
                console.print(f"[red][server][/red] Error: {e}")

                # Clean up tasks
                if server:
                    server.should_exit = True

                if server_task and not server_task.done():
                    server_task.cancel()
                    try:
                        await server_task
                    except asyncio.CancelledError:
                        pass

                if watch_task and not watch_task.done():
                    watch_task.cancel()
                    try:
                        await watch_task
                    except asyncio.CancelledError:
                        pass

                # Wait a bit before retrying
                await asyncio.sleep(1)

    # Run backend with retry logic
    await run_backend_with_retry()


# === Token Management Utilities ===


def save_token_id(app_dir: Path, token_id: str):
    """Save token ID to project.json.

    Args:
        app_dir: Application directory
        token_id: Databricks token ID
    """
    project_json_path = app_dir / ".apx" / "project.json"
    ensure_dir(app_dir / ".apx")

    try:
        config = read_project_config(project_json_path)
    except (FileNotFoundError, Exception):
        # If file doesn't exist or is corrupted, create new config
        config = ProjectConfig()

    config.dev.token_id = token_id
    write_project_config(project_json_path, config)


def get_token_id(app_dir: Path) -> str | None:
    """Get token ID from project.json.

    Args:
        app_dir: Application directory

    Returns:
        Token ID or None if not found
    """
    project_json_path = app_dir / ".apx" / "project.json"

    if project_json_path.exists():
        try:
            config = read_project_config(project_json_path)
            return config.dev.token_id
        except Exception:
            pass

    return None


def save_token_to_keyring(keyring_id: str, token_value: str):
    """Save token to system keyring.

    Args:
        keyring_id: Keyring identifier (project path)
        token_value: Token value to store
    """
    keyring.set_password("apx-dev", keyring_id, token_value)


def get_token_from_keyring(keyring_id: str) -> str | None:
    """Get token from system keyring.

    Args:
        keyring_id: Keyring identifier (project path)

    Returns:
        Token value or None if not found
    """
    return keyring.get_password("apx-dev", keyring_id)


def delete_token_from_keyring(keyring_id: str):
    """Delete token from system keyring.

    Args:
        keyring_id: Keyring identifier (project path)
    """
    try:
        keyring.delete_password("apx-dev", keyring_id)
    except Exception:
        # Password might not exist, that's fine
        pass


async def run_frontend_with_logging(
    app_dir: Path,
    port: int,
    max_retries: int = 10,
    state: FrontendProcessState | None = None,
) -> None:
    """Run frontend dev server and capture output to in-memory buffer.

    Args:
        app_dir: Application directory
        port: Frontend port
        max_retries: Maximum number of retry attempts
        state: Optional ServerState object to store process reference
    """
    # Use the already-configured logger (set up by dev_server)
    logger = logging.getLogger("apx.frontend")

    # Setup retry logger to use same handler
    retry_logger = logging.getLogger("apx.retry")
    retry_logger.setLevel(logging.INFO)
    retry_logger.handlers.clear()
    if logger.handlers:
        retry_logger.addHandler(logger.handlers[0])
    retry_logger.propagate = False

    @retry(
        stop=stop_after_attempt(max_retries),
        wait=wait_exponential(multiplier=1, min=2, max=60),
        before_sleep=log_retry_attempt,
        retry=retry_if_not_exception_type(RuntimeError),
        reraise=True,
    )
    async def run_with_retry() -> None:
        """Frontend runner with retry logic."""
        logger.info(f"Starting frontend server on port {port}")

        # Create process group/session so we can stop the full vite tree reliably.
        creationflags = 0
        start_new_session = False
        if os.name == "nt":
            creationflags = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
        else:
            start_new_session = True

        process = await asyncio.create_subprocess_exec(
            "bun",
            "run",
            "dev",
            cwd=app_dir,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=start_new_session,
            creationflags=creationflags,
        )

        # Store process reference in state if provided
        if state is not None:
            state.frontend_process = process
            # Track process metadata immediately (bun may hand off to node and exit quickly).
            try:
                from apx.cli.dev.process_control import track_process as _track

                state.frontend_tracked = _track(process.pid)
            except Exception:
                state.frontend_tracked = None

        async def read_stream(stream, stream_name):
            """Read from stream and log each line."""
            async for line in stream:
                try:
                    decoded_line = line.decode("utf-8", errors="replace").strip()
                    if decoded_line:
                        logger.info(f"{stream_name} | {decoded_line}")
                except Exception:
                    pass
                # Small delay to prevent excessive I/O
                await asyncio.sleep(0.01)

        try:
            # Read both stdout and stderr
            await asyncio.gather(
                read_stream(process.stdout, "stdout"),
                read_stream(process.stderr, "stderr"),
            )

            await process.wait()

            # Check exit code
            if process.returncode != 0:
                logger.error(f"Frontend process exited with code {process.returncode}")
                raise RuntimeError(
                    f"Frontend process failed with exit code {process.returncode}"
                )
        except asyncio.CancelledError:
            # Process will be killed by the caller
            raise

    # Run with retry
    await run_with_retry()


async def run_openapi_with_logging(app_dir: Path, max_retries: int = 10):
    """Run OpenAPI watcher and capture output to in-memory buffer.

    Args:
        app_dir: Application directory
        max_retries: Maximum number of retry attempts
    """
    from apx.openapi import _openapi_watch

    # Use the already-configured logger (set up by dev_server)
    logger = logging.getLogger("apx.openapi")

    # Setup retry logger to use same handler
    retry_logger = logging.getLogger("apx.retry")
    retry_logger.setLevel(logging.INFO)
    retry_logger.handlers.clear()
    if logger.handlers:
        retry_logger.addHandler(logger.handlers[0])
    retry_logger.propagate = False

    @retry(
        stop=stop_after_attempt(max_retries),
        wait=wait_exponential(multiplier=1, min=2, max=60),
        before_sleep=log_retry_attempt,
        reraise=True,
    )
    async def run_with_retry():
        """OpenAPI watcher with retry logic."""
        logger.info("Starting OpenAPI watcher")

        # Note: We don't redirect stdout/stderr here because the backend process
        # already handles that. The OpenAPI watcher uses the logger directly.
        try:
            # Run the OpenAPI watcher with logger
            await _openapi_watch(app_dir, logger=logger)
        except Exception as e:
            logger.error(f"OpenAPI watcher failed: {e}")
            raise

    # Run with retry
    await run_with_retry()


# === DevManager Class ===


class DevManager:
    """Manages development server processes."""

    def __init__(self, app_dir: Path):
        """Initialize the DevManager with an app directory.

        Args:
            app_dir: The path to the application directory
        """
        self.app_dir: Path = app_dir
        self.apx_dir: Path = app_dir / ".apx"
        self.project_json_path: Path = self.apx_dir / "project.json"
        self.socket_path: Path = self.apx_dir / "dev.sock"

    def get_or_create_config(self) -> ProjectConfig:
        """Get or create project configuration."""
        ensure_dir(self.apx_dir)

        if self.project_json_path.exists():
            try:
                return read_project_config(self.project_json_path)
            except Exception:
                pass

        # Create new config
        config = ProjectConfig()
        write_project_config(self.project_json_path, config)
        return config

    def is_dev_server_running(self) -> bool:
        """Check if the dev server is running by checking socket existence."""
        return self.socket_path.exists()

    def start(
        self,
        frontend_port: int = 5173,
        backend_port: int = 8000,
        host: str = "localhost",
        obo: bool = True,
        openapi: bool = True,
        max_retries: int = 10,
        watch: bool = False,
    ):
        """Start development server in detached mode.

        Args:
            frontend_port: Port for the frontend development server
            backend_port: Port for the backend server
            host: Host for dev, frontend, and backend servers
            obo: Whether to add On-Behalf-Of header to the backend server
            openapi: Whether to start OpenAPI watcher process
            max_retries: Maximum number of retry attempts for processes
            watch: Whether in watch mode or detached mode
        """
        # Check if dev server is already running
        if self.is_dev_server_running():
            console.print(
                "[yellow]⚠️  Dev server is already running. Run 'apx dev stop' first.[/yellow]"
            )
            raise Exit(code=1)

        # If we have previous process metadata in project.json, we only stop processes we tracked.
        # This avoids kill-by-name cleanups that might terminate unrelated processes.
        config = self.get_or_create_config()
        if config.dev.frontend_process or config.dev.dev_server_process:
            console.print(
                "[cyan]🧹 Found previous dev session state; attempting safe cleanup of tracked processes...[/cyan]"
            )
            # Prefer pgid-based shutdown (works even if bun pid is gone).
            if config.dev.frontend_process:
                stop_tracked_process(
                    TrackedProcess(
                        pid=config.dev.frontend_process.pid,
                        create_time=config.dev.frontend_process.create_time,
                        pgid=config.dev.frontend_process.pgid,
                    ),
                    name="frontend",
                    sigint_timeout=0.2,
                    sigterm_timeout=0.6,
                    sigkill_timeout=0.6,
                )
                wait_for_no_descendants(
                    TrackedProcess(
                        pid=config.dev.frontend_process.pid,
                        create_time=config.dev.frontend_process.create_time,
                        pgid=config.dev.frontend_process.pgid,
                    ),
                    timeout=2.0,
                    poll=0.1,
                )

                # Port-aware last resort: if the previous frontend port is still held,
                # kill only listener PIDs that look like they belong to this app.
                if config.dev.frontend_port is not None:
                    if not wait_for_port_free(
                        is_port_available_fn=is_port_available,
                        port=config.dev.frontend_port,
                        timeout=1.5,
                        poll=0.1,
                    ):
                        listeners = find_listeners_for_port(config.dev.frontend_port)
                        to_kill = pids_belong_to_app(
                            listeners,
                            app_dir=self.app_dir,
                            expected_pgid=config.dev.frontend_process.pgid,
                        )
                        if to_kill:
                            kill_pids(
                                to_kill, name="frontend-listener", sig=signal.SIGKILL
                            )
                        if not wait_for_port_free(
                            is_port_available_fn=is_port_available,
                            port=config.dev.frontend_port,
                            timeout=1.5,
                            poll=0.1,
                        ):
                            listeners2 = find_listeners_for_port(
                                config.dev.frontend_port
                            )
                            console.print(
                                f"[red]❌ Could not free previous frontend port {config.dev.frontend_port} "
                                f"(listening PIDs: {listeners2})[/red]"
                            )
                            raise Exit(code=1)
            if config.dev.dev_server_process:
                stop_tracked_process(
                    TrackedProcess(
                        pid=config.dev.dev_server_process.pid,
                        create_time=config.dev.dev_server_process.create_time,
                        pgid=config.dev.dev_server_process.pgid,
                    ),
                    name="dev-server",
                    sigint_timeout=0.2,
                    sigterm_timeout=0.6,
                    sigkill_timeout=0.6,
                )
                wait_for_no_descendants(
                    TrackedProcess(
                        pid=config.dev.dev_server_process.pid,
                        create_time=config.dev.dev_server_process.create_time,
                        pgid=config.dev.dev_server_process.pgid,
                    ),
                    timeout=2.0,
                    poll=0.1,
                )

        # Find available ports
        console.print("[cyan]🔍 Finding available ports...[/cyan]")

        # Find frontend/vite server port (5173-5200)
        available_frontend_port = find_available_port(5173, 5200)
        if available_frontend_port is None:
            console.print(
                "[red]❌ No available ports found for frontend server in range 5173-5200[/red]"
            )
            raise Exit(code=1)

        # Find backend/app server port (8000-8040)
        available_backend_port = find_available_port(8000, 8040)
        if available_backend_port is None:
            console.print(
                "[red]❌ No available ports found for backend server in range 8000-8040[/red]"
            )
            raise Exit(code=1)

        # Use the found ports instead of the provided/default values
        frontend_port = available_frontend_port
        backend_port = available_backend_port

        console.print(
            f"[green]✓[/green] Found available ports - Frontend: {frontend_port}, Backend: {backend_port}"
        )
        console.print()

        mode_msg = (
            "🚀 Starting development server in watch mode..."
            if watch
            else "🚀 Starting development server in detached mode..."
        )

        console.print(f"[bold chartreuse1]{mode_msg}[/bold chartreuse1]")
        console.print(f"[cyan]Dev Socket:[/cyan] {self.socket_path}")
        console.print(f"[cyan]Frontend:[/cyan] http://localhost:{frontend_port}")
        console.print(f"[green]Backend:[/green] http://{host}:{backend_port}")
        console.print()

        # Start the dev server process
        popen_kwargs: dict[str, Any] = {}
        if os.name == "nt":
            popen_kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
        else:
            popen_kwargs["start_new_session"] = True

        dev_server_proc = subprocess.Popen(
            [
                "uv",
                "run",
                "apx",
                "dev",
                "_run_server",
                str(self.app_dir),
                str(self.socket_path),
                str(frontend_port),
                str(backend_port),
                host,
                str(obo).lower(),
                str(openapi).lower(),
                str(max_retries),
            ],
            cwd=self.app_dir,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **popen_kwargs,
        )

        # Persist dev server process metadata into project.json for robust stop fallback.
        dev_tp = track_process(dev_server_proc.pid)
        config = self.get_or_create_config()
        config.dev.host = host
        config.dev.frontend_port = frontend_port
        config.dev.backend_port = backend_port
        if dev_tp is not None:
            config.dev.dev_server_process = DevProcessInfo(
                pid=dev_tp.pid,
                create_time=dev_tp.create_time,
                pgid=dev_tp.pgid,
            )
        else:
            config.dev.dev_server_process = None
        config.dev.frontend_process = None
        write_project_config(self.project_json_path, config)

        console.print("[cyan]✓[/cyan] Dev server started")
        console.print()

        # Wait a moment for server to start and create socket
        max_wait = 5  # seconds
        for _ in range(max_wait * 10):
            if self.socket_path.exists():
                break
            time.sleep(0.1)
        else:
            console.print(
                "[red]❌ Dev server did not create socket within timeout[/red]"
            )
            # Try to kill the process if it's still running
            try:
                dev_server_proc.terminate()
                dev_server_proc.wait(timeout=2)
            except Exception:
                pass
            raise Exit(code=1)

        # Send start request to dev server using the client
        client = DevServerClient(self.socket_path)

        try:
            request = ActionRequest(
                frontend_port=frontend_port,
                backend_port=backend_port,
                host=host,
                obo=obo,
                openapi=openapi,
                max_retries=max_retries,
            )
            response = client.start(request)

            if response.status == "success":
                console.print(
                    "[bold green]✨ Development servers started successfully![/bold green]"
                )
            else:
                console.print(f"[yellow]⚠️  Warning: {response.message}[/yellow]")
        except Exception as e:
            console.print(
                f"[yellow]⚠️  Warning: Could not connect to dev server: {e}[/yellow]"
            )

        if not watch:
            console.print(
                "[dim]Run 'apx dev status' to check status or 'apx dev stop' to stop the servers.[/dim]"
            )

    def status(self):
        """Check the status of development servers."""
        # Check if dev server is running
        if not self.is_dev_server_running():
            console.print("[yellow]No development server found.[/yellow]")
            console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
            return

        # Query dev server for status using the client
        client = DevServerClient(self.socket_path)

        try:
            status_data = client.status()

            # Create a status table
            table = Table(
                title="Development Server Status",
                show_header=True,
                header_style="bold magenta",
            )
            table.add_column("Process", style="cyan", width=12)
            table.add_column("Status", justify="center")
            table.add_column("Port", justify="right", style="green")

            # Dev server row
            table.add_row(
                "Dev Server",
                "[green]●[/green] Running",
                "Unix Socket",
            )

            # Frontend row
            frontend_status = (
                "[green]●[/green] Running"
                if status_data.frontend_running
                else "[red]●[/red] Stopped"
            )
            table.add_row(
                "Frontend",
                frontend_status,
                str(status_data.frontend_port),
            )

            # Backend row
            backend_status = (
                "[green]●[/green] Running"
                if status_data.backend_running
                else "[red]●[/red] Stopped"
            )
            table.add_row(
                "Backend",
                backend_status,
                str(status_data.backend_port),
            )

            # OpenAPI row
            openapi_status = (
                "[green]●[/green] Running"
                if status_data.openapi_running
                else "[red]●[/red] Stopped"
            )
            table.add_row("OpenAPI", openapi_status, "-")

            console.print(table)
            console.print()
            console.print(f"[dim]Dev Server Socket: {self.socket_path}[/dim]")
            console.print(
                "[dim]Use 'apx dev logs' to view logs or 'apx dev logs -f' to stream continuously.[/dim]"
            )
        except Exception as e:
            console.print(f"[yellow]⚠️  Could not connect to dev server: {e}[/yellow]")

    def stop(self):
        """Stop development server.

        Stops servers deterministically by targeting only processes we started.
        """
        if not self.is_dev_server_running():
            console.print("[yellow]No development server found.[/yellow]")
            return

        console.print("[bold yellow]Stopping development server...[/bold yellow]")

        # Capture project.json state early (dev server may be unresponsive / exiting).
        config_before = self.get_or_create_config()
        frontend_port = config_before.dev.frontend_port
        backend_port = config_before.dev.backend_port

        # Try to send stop request to dev server first (graceful stop path).
        client = DevServerClient(self.socket_path)

        try:
            response = client.stop()
            if response.status == "success":
                console.print("[green]✓[/green] Stopped all servers via API")
            else:
                raise RuntimeError(response.message)
        except Exception as e:
            # API path failed; fall back to project.json-targeted shutdown.
            console.print(
                f"[yellow]⚠️  Dev server API stop failed; falling back to process cleanup: {e}[/yellow]"
            )
            config = self.get_or_create_config()
            did_any = False
            if config.dev.frontend_process:
                stop_tracked_process(
                    TrackedProcess(
                        pid=config.dev.frontend_process.pid,
                        create_time=config.dev.frontend_process.create_time,
                        pgid=config.dev.frontend_process.pgid,
                    ),
                    name="frontend",
                    sigint_timeout=0.2,
                    sigterm_timeout=0.6,
                    sigkill_timeout=0.6,
                )
                wait_for_no_descendants(
                    TrackedProcess(
                        pid=config.dev.frontend_process.pid,
                        create_time=config.dev.frontend_process.create_time,
                        pgid=config.dev.frontend_process.pgid,
                    ),
                    timeout=2.0,
                    poll=0.1,
                )
                did_any = True

            if config.dev.dev_server_process:
                stop_tracked_process(
                    TrackedProcess(
                        pid=config.dev.dev_server_process.pid,
                        create_time=config.dev.dev_server_process.create_time,
                        pgid=config.dev.dev_server_process.pgid,
                    ),
                    name="dev-server",
                    sigint_timeout=0.2,
                    sigterm_timeout=0.6,
                    sigkill_timeout=0.6,
                )
                wait_for_no_descendants(
                    TrackedProcess(
                        pid=config.dev.dev_server_process.pid,
                        create_time=config.dev.dev_server_process.create_time,
                        pgid=config.dev.dev_server_process.pgid,
                    ),
                    timeout=2.0,
                    poll=0.1,
                )
                did_any = True

            if not did_any:
                # Last-resort: only attempt to stop the dev server process by cmdline.
                # We intentionally do NOT kill bun/vite/node by name.
                cleanup_dev_server_processes(self.app_dir, silent=True)

        # Wait for socket to be removed (whether by API or by our cleanup)
        max_wait = 3  # seconds
        for _ in range(max_wait * 10):
            if not self.socket_path.exists():
                break
            time.sleep(0.1)

        # Force remove socket if it still exists
        if self.socket_path.exists():
            self.socket_path.unlink(missing_ok=True)

        # Verify ports are free (from captured pidfile if available).
        if frontend_port is not None:
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=frontend_port,
                timeout=2.0,
                poll=0.1,
            ):
                listeners = find_listeners_for_port(frontend_port)
                expected_pgid = (
                    config_before.dev.frontend_process.pgid
                    if config_before.dev.frontend_process is not None
                    else None
                )
                to_kill = pids_belong_to_app(
                    listeners, app_dir=self.app_dir, expected_pgid=expected_pgid
                )
                if to_kill:
                    kill_pids(to_kill, name="frontend-listener", sig=signal.SIGKILL)
                if not wait_for_port_free(
                    is_port_available_fn=is_port_available,
                    port=frontend_port,
                    timeout=1.5,
                    poll=0.1,
                ):
                    listeners2 = find_listeners_for_port(frontend_port)
                    console.print(
                        f"[red]❌ Frontend port {frontend_port} is still in use "
                        f"(listening PIDs: {listeners2})[/red]"
                    )
                    raise Exit(code=1)
        if backend_port is not None:
            if not wait_for_port_free(
                is_port_available_fn=is_port_available,
                port=backend_port,
                timeout=2.0,
                poll=0.1,
            ):
                listeners = find_listeners_for_port(backend_port)
                console.print(
                    f"[red]❌ Backend port {backend_port} is still in use "
                    f"(listening PIDs: {listeners})[/red]"
                )
                raise Exit(code=1)

        # Clear stored process metadata (token_id remains).
        config = self.get_or_create_config()
        config.dev.dev_server_process = None
        config.dev.frontend_process = None
        write_project_config(self.project_json_path, config)

        console.print()
        console.print(
            "[bold green]✨ Development server stopped successfully![/bold green]"
        )
        console.print("[dim]Token remains valid in keyring until expiration[/dim]")

    def stream_logs(
        self,
        duration_seconds: int | None = None,
        ui_only: bool = False,
        backend_only: bool = False,
        openapi_only: bool = False,
        app_only: bool = False,
        raw_output: bool = False,
        follow: bool = False,
        timeout_seconds: int | None = None,
    ):
        """Stream logs from dev server using SSE.

        Args:
            duration_seconds: Show logs from last N seconds (None = all logs from buffer)
            ui_only: Only show frontend logs
            backend_only: Only show backend logs
            openapi_only: Only show OpenAPI logs
            app_only: Only show application logs (from your app code)
            raw_output: Show raw log output without prefix formatting
            follow: Continue streaming new logs (like tail -f). If False, exits after initial logs.
            timeout_seconds: Stop streaming after N seconds (None = indefinite)
        """
        if not self.is_dev_server_running():
            console.print("[yellow]No development server found.[/yellow]")
            return

        # Determine process filter
        # Note: app_only is handled client-side because it's a subset of backend logs
        process_filter: Literal["frontend", "backend", "openapi", "all"] = "all"
        if ui_only and not backend_only and not openapi_only and not app_only:
            process_filter = "frontend"
        elif backend_only and not ui_only and not openapi_only and not app_only:
            process_filter = "backend"
        elif openapi_only and not ui_only and not backend_only and not app_only:
            process_filter = "openapi"
        elif app_only and not ui_only and not backend_only and not openapi_only:
            # For app-only, we need backend logs and will filter client-side
            process_filter = "backend"

        # Connect to SSE endpoint using the client
        client = DevServerClient(self.socket_path)

        log_count = 0  # Initialize early to avoid unbound error

        try:
            with client.stream_logs(
                duration=duration_seconds,
                process=process_filter,
            ) as log_stream:
                start_time = time.time()

                for item in log_stream:
                    # Check timeout
                    if (
                        timeout_seconds
                        and (time.time() - start_time) >= timeout_seconds
                    ):
                        if follow:
                            console.print(
                                "\n[dim]Timeout reached, stopping stream.[/dim]"
                            )
                        break

                    # Handle sentinel event for end of buffered logs
                    if item == StreamEvent.BUFFERED_DONE:
                        if not follow:
                            # Stop streaming after buffered logs if not following
                            break
                        # Otherwise, continue to stream new logs
                        continue

                    # Must be a LogEntry at this point
                    if not isinstance(item, LogEntry):
                        continue

                    # Client-side filtering for app-only logs
                    if app_only:
                        # Only show backend logs that have "APP | " prefix
                        if item.process_name != "backend":
                            continue
                        if not item.content.startswith("APP | "):
                            continue

                    print_log_entry(item.model_dump(), raw_output=raw_output)
                    log_count += 1

        except KeyboardInterrupt:
            if follow:
                console.print("\n[dim]Stopped streaming logs.[/dim]")
        except Exception as e:
            console.print(f"\n[red]Error streaming logs: {e}[/red]")

        # Print summary for non-follow mode
        if not follow:
            if log_count > 0:
                console.print(f"\n[dim]Showed {log_count} log entries[/dim]")
            else:
                console.print("[dim]No logs found[/dim]")
