"""Development server utilities for apx."""

import asyncio
import contextlib
import importlib
import io
import json
import logging
import psutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import keyring
from databricks.sdk import WorkspaceClient
from dotenv import load_dotenv
from fastapi import FastAPI, Request
from pydantic import BaseModel, Field
from rich.table import Table
from starlette.middleware.base import BaseHTTPMiddleware
from tenacity import (
    RetryCallState,
    retry,
    stop_after_attempt,
    wait_exponential,
)
from typer import Exit
import watchfiles
import uvicorn
from apx.utils import (
    console,
    PrefixedLogHandler,
    ensure_dir,
)
from apx import __version__


# note: header name must be lowercase and with - symbols
ACCESS_TOKEN_HEADER_NAME = "x-forwarded-access-token"


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


# === Pydantic Models for Project Configuration ===


class ProcessInfo(BaseModel):
    """Information about a running process."""

    pid: int
    port: int | None = None
    created_at: str = Field(default_factory=lambda: time.strftime("%Y-%m-%d %H:%M:%S"))


class ProjectConfig(BaseModel):
    """Configuration stored in .apx/project.json."""

    token_id: str | None = None
    dev_server_pid: int | None = None
    dev_server_port: int | None = None
    processes: dict[str, ProcessInfo] = Field(default_factory=dict)

    @classmethod
    def read_from_file(cls, file_path: Path) -> "ProjectConfig":
        """Read project config from file.

        Args:
            file_path: Path to project.json

        Returns:
            ProjectConfig instance
        """
        if not file_path.exists():
            raise FileNotFoundError(f"Project config not found at {file_path}")

        data = json.loads(file_path.read_text())
        return cls(**data)

    def write_to_file(self, file_path: Path):
        """Write project config to file.

        Args:
            file_path: Path to project.json
        """
        ensure_dir(file_path.parent)
        file_path.write_text(self.model_dump_json(indent=2))

    def update_to_file(self, file_path: Path):
        """Update project config in file (convenience method).

        Args:
            file_path: Path to project.json
        """
        self.write_to_file(file_path)


@contextlib.contextmanager
def suppress_output_and_logs():
    """Suppress stdout, stderr and logging output temporarily."""
    # Save original stdout/stderr
    old_stdout = sys.stdout
    old_stderr = sys.stderr

    # Save original log level for root logger and all existing loggers
    root_logger = logging.getLogger()
    original_root_level = root_logger.level
    original_levels = {}

    for name in logging.Logger.manager.loggerDict:
        logger = logging.getLogger(name)
        if hasattr(logger, "level"):
            original_levels[name] = logger.level

    try:
        # Redirect stdout/stderr to devnull
        sys.stdout = io.StringIO()
        sys.stderr = io.StringIO()

        # Set all loggers to CRITICAL to suppress INFO/WARNING logs
        root_logger.setLevel(logging.CRITICAL)
        for name in original_levels:
            logging.getLogger(name).setLevel(logging.CRITICAL)

        yield
    finally:
        # Restore stdout/stderr
        sys.stdout = old_stdout
        sys.stderr = old_stderr

        # Restore log levels
        root_logger.setLevel(original_root_level)
        for name, level in original_levels.items():
            logging.getLogger(name).setLevel(level)


def load_app(app_module_name: str, reload_modules: bool = False) -> FastAPI:
    """Load and return the FastAPI app instance."""
    # Split the app_name into module path and attribute name
    if ":" not in app_module_name:
        console.print(
            "[red]❌ Invalid app module format. Expected format: some.package.file:app[/red]"
        )
        raise Exit(code=1)

    module_path, attribute_name = app_module_name.split(":", 1)

    # If reloading, clear the module and all its submodules from cache
    if reload_modules:
        # Find all modules that start with the base module path
        base_path = module_path.split(".")[0]
        modules_to_delete = [
            name
            for name in sys.modules.keys()
            if name.startswith(base_path + ".") or name == base_path
        ]
        for mod_name in modules_to_delete:
            del sys.modules[mod_name]

    # Import the module
    try:
        module = importlib.import_module(module_path)
    except ImportError as e:
        console.print(f"[red]❌ Failed to import module {module_path}: {e}[/red]")
        raise Exit(code=1)

    # Get the app attribute from the module
    try:
        app_instance = getattr(module, attribute_name)
    except AttributeError:
        console.print(
            f"[red]❌ Module {module_path} does not have attribute '{attribute_name}'[/red]"
        )
        raise Exit(code=1)

    if not isinstance(app_instance, FastAPI):
        console.print(
            f"[red]❌ '{attribute_name}' is not a FastAPI app instance.[/red]"
        )
        raise Exit(code=1)

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


class DevServerAccessLogFilter(logging.Filter):
    """Filter to exclude dev server internal API logs from access logs."""

    def filter(self, record):
        """Return False for dev server internal endpoints to exclude them from logs."""
        message = record.getMessage()
        # Exclude logs for dev server internal endpoints
        internal_paths = ["/logs", "/status", "/start", "/stop", "/restart"]
        for path in internal_paths:
            if f'"{path}' in message or f"'{path}" in message:
                return False
        return True


def setup_uvicorn_logging(use_memory: bool = False):
    """Configure uvicorn loggers to use in-memory buffer or console.

    Args:
        use_memory: If True, use the in-memory logger (set up by dev_server)
                    If False, use console logging (for standalone mode)
    """
    # Configure ONLY uvicorn loggers
    for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
        logger = logging.getLogger(logger_name)
        logger.handlers.clear()

        # Add filter to uvicorn.access to exclude dev server internal API logs
        if logger_name == "uvicorn.access":
            logger.addFilter(DevServerAccessLogFilter())

        if use_memory:
            # Use the backend logger that's already configured by dev_server
            backend_logger = logging.getLogger("apx.backend")
            if backend_logger.handlers:
                # Create a wrapper handler that adds "BE | " prefix
                class PrefixedHandler(logging.Handler):
                    def __init__(self, base_handler, prefix):
                        super().__init__()
                        self.base_handler = base_handler
                        self.prefix = prefix

                    def emit(self, record):
                        # Add prefix to the message
                        original_msg = record.getMessage()
                        record.msg = f"{self.prefix} | {original_msg}"
                        record.args = ()
                        self.base_handler.emit(record)

                # Add the prefixed handler
                for handler in backend_logger.handlers:
                    prefixed_handler = PrefixedHandler(handler, "BE")
                    logger.addHandler(prefixed_handler)
            else:
                # Fallback to console if no handlers found
                handler = PrefixedLogHandler(prefix="[backend]", color="aquamarine1")
                formatter = logging.Formatter("%(message)s")
                handler.setFormatter(formatter)
                logger.addHandler(handler)
        else:
            # Console logging (no buffering needed)
            handler = PrefixedLogHandler(prefix="[backend]", color="aquamarine1")
            formatter = logging.Formatter("%(message)s")
            handler.setFormatter(formatter)
            logger.addHandler(handler)

        logger.setLevel(logging.INFO)
        logger.propagate = False


async def run_backend(
    cwd: Path,
    app_module_name: str,
    backend_host: str,
    backend_port: int,
    obo: bool = False,
    log_file: Path | None = None,
    max_retries: int = 10,
):
    """Run the backend server programmatically with uvicorn and hot-reload support.

    Args:
        cwd: Current working directory
        app_module_name: Module name for the FastAPI app
        backend_host: Host to bind to
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
            backend_logger.info(
                f"Starting backend server on {backend_host}:{backend_port}"
            )
        else:
            console.print(
                f"[green][server][/]Starting server on {backend_host}:{backend_port} from app: {app_module_name}"
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
                    host=backend_host,
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
        config = ProjectConfig.read_from_file(project_json_path)
    except (FileNotFoundError, Exception):
        # If file doesn't exist or is corrupted, create new config
        config = ProjectConfig()

    config.token_id = token_id
    config.write_to_file(project_json_path)


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
            config = ProjectConfig.read_from_file(project_json_path)
            return config.token_id
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


async def run_frontend_with_logging(app_dir: Path, port: int, max_retries: int = 10):
    """Run frontend dev server and capture output to in-memory buffer.

    Args:
        app_dir: Application directory
        port: Frontend port
        max_retries: Maximum number of retry attempts
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
        reraise=True,
    )
    async def run_with_retry():
        """Frontend runner with retry logic."""
        logger.info(f"Starting frontend server on port {port}")

        process = await asyncio.create_subprocess_exec(
            "bun",
            "run",
            "dev",
            cwd=app_dir,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )

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

    def _get_or_create_config(self) -> ProjectConfig:
        """Get or create project configuration."""
        ensure_dir(self.apx_dir)

        if self.project_json_path.exists():
            try:
                return ProjectConfig.read_from_file(self.project_json_path)
            except Exception:
                pass

        # Create new config
        config = ProjectConfig()
        config.write_to_file(self.project_json_path)
        return config

    def _save_process_pid(self, name: str, pid: int, port: int | None = None):
        """Save a process PID to project.json."""
        config = self._get_or_create_config()
        config.processes[name] = ProcessInfo(pid=pid, port=port)
        config.write_to_file(self.project_json_path)

    def _get_process_info(self, name: str) -> tuple[int, int | None] | None:
        """Get process PID and port from project.json. Returns (pid, port) or None."""
        if not self.project_json_path.exists():
            return None

        try:
            config = ProjectConfig.read_from_file(self.project_json_path)
            if name in config.processes:
                proc_info = config.processes[name]
                return (proc_info.pid, proc_info.port)
        except Exception:
            pass

        return None

    def _get_all_processes(self) -> list[tuple[str, int, int | None]]:
        """Get all process info from project.json. Returns list of (name, pid, port)."""
        if not self.project_json_path.exists():
            return []

        try:
            config = ProjectConfig.read_from_file(self.project_json_path)
            return [
                (name, proc_info.pid, proc_info.port)
                for name, proc_info in config.processes.items()
            ]
        except Exception:
            return []

    def _remove_process_pid(self, name: str):
        """Remove a process PID from project.json."""
        if not self.project_json_path.exists():
            return

        try:
            config = ProjectConfig.read_from_file(self.project_json_path)
            if name in config.processes:
                del config.processes[name]
                config.write_to_file(self.project_json_path)
        except Exception:
            pass

    def _is_process_running(self, pid: int) -> bool:
        """Check if a process with the given PID is running."""
        try:
            process = psutil.Process(pid)
            return process.is_running()
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            return False

    def start(
        self,
        frontend_port: int = 5173,
        backend_port: int = 8000,
        backend_host: str = "0.0.0.0",
        obo: bool = True,
        openapi: bool = True,
        max_retries: int = 10,
        dev_server_port: int = 8040,
    ):
        """Start development server in detached mode.

        Args:
            frontend_port: Port for the frontend development server
            backend_port: Port for the backend server
            backend_host: Host for the backend server
            obo: Whether to add On-Behalf-Of header to the backend server
            openapi: Whether to start OpenAPI watcher process
            max_retries: Maximum number of retry attempts for processes
            dev_server_port: Port for the dev server
        """
        # Check if dev server is already running
        config = self._get_or_create_config()
        if config.dev_server_pid and self._is_process_running(config.dev_server_pid):
            console.print(
                f"[yellow]⚠️  Dev server is already running (PID: {config.dev_server_pid}). Run 'apx dev stop' first.[/yellow]"
            )
            raise Exit(code=1)

        console.print(
            "[bold chartreuse1]🚀 Starting development server in detached mode...[/bold chartreuse1]"
        )
        console.print(f"[cyan]Dev Server:[/cyan] http://localhost:{dev_server_port}")
        console.print(f"[cyan]Frontend:[/cyan] http://localhost:{frontend_port}")
        console.print(f"[green]Backend:[/green] http://{backend_host}:{backend_port}")
        console.print()

        # Start the dev server process
        dev_server_proc = subprocess.Popen(
            [
                "uv",
                "run",
                "apx",
                "dev",
                "_run_server",
                str(self.app_dir),
                str(dev_server_port),
                str(frontend_port),
                str(backend_port),
                backend_host,
                str(obo).lower(),
                str(openapi).lower(),
                str(max_retries),
            ],
            cwd=self.app_dir,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )

        # Save dev server info
        config.dev_server_pid = dev_server_proc.pid
        config.dev_server_port = dev_server_port
        config.write_to_file(self.project_json_path)

        console.print(f"[cyan]✓[/cyan] Dev server started (PID: {dev_server_proc.pid})")
        console.print()

        # Wait a moment for server to start
        import time

        time.sleep(2)

        # Send start request to dev server
        import requests

        try:
            response = requests.post(
                f"http://localhost:{dev_server_port}/actions/start",
                json={
                    "frontend_port": frontend_port,
                    "backend_port": backend_port,
                    "backend_host": backend_host,
                    "obo": obo,
                    "openapi": openapi,
                    "max_retries": max_retries,
                },
                timeout=5,
            )

            if response.status_code == 200:
                console.print(
                    "[bold green]✨ Development servers started successfully![/bold green]"
                )
            else:
                console.print(
                    f"[yellow]⚠️  Warning: Dev server responded with status {response.status_code}[/yellow]"
                )
        except Exception as e:
            console.print(
                f"[yellow]⚠️  Warning: Could not connect to dev server: {e}[/yellow]"
            )

        console.print(
            "[dim]Run 'apx dev status' to check status or 'apx dev stop' to stop the servers.[/dim]"
        )

    def status(self):
        """Check the status of development servers."""
        if not self.project_json_path.exists():
            console.print("[yellow]No development servers found.[/yellow]")
            console.print("[dim]Run 'apx dev start' to start the servers.[/dim]")
            return

        config = self._get_or_create_config()

        if not config.dev_server_pid:
            console.print("[yellow]No development server found.[/yellow]")
            console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
            return

        # Check if dev server is running
        if not self._is_process_running(config.dev_server_pid):
            console.print(
                f"[red]Dev server (PID: {config.dev_server_pid}) is not running.[/red]"
            )
            console.print("[dim]Run 'apx dev start' to start the server.[/dim]")
            return

        # Query dev server for status
        import requests

        try:
            response = requests.get(
                f"http://localhost:{config.dev_server_port}/status",
                timeout=5,
            )

            if response.status_code == 200:
                status_data = response.json()

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
                    str(config.dev_server_port),
                )

                # Frontend row
                frontend_status = (
                    "[green]●[/green] Running"
                    if status_data["frontend_running"]
                    else "[red]●[/red] Stopped"
                )
                table.add_row(
                    "Frontend",
                    frontend_status,
                    str(status_data["frontend_port"]),
                )

                # Backend row
                backend_status = (
                    "[green]●[/green] Running"
                    if status_data["backend_running"]
                    else "[red]●[/red] Stopped"
                )
                table.add_row(
                    "Backend",
                    backend_status,
                    str(status_data["backend_port"]),
                )

                # OpenAPI row
                openapi_status = (
                    "[green]●[/green] Running"
                    if status_data["openapi_running"]
                    else "[red]●[/red] Stopped"
                )
                table.add_row("OpenAPI", openapi_status, "-")

                console.print(table)
                console.print()
                console.print(f"[dim]Dev Server PID: {config.dev_server_pid}[/dim]")
                console.print(
                    "[dim]Use 'apx dev logs' to view logs or 'apx dev logs -f' to stream continuously.[/dim]"
                )
            else:
                console.print(
                    f"[yellow]⚠️  Dev server responded with status {response.status_code}[/yellow]"
                )
        except Exception as e:
            console.print(f"[yellow]⚠️  Could not connect to dev server: {e}[/yellow]")

    def stop(self):
        """Stop development server."""
        if not self.project_json_path.exists():
            console.print("[yellow]No development server found.[/yellow]")
            return

        config = self._get_or_create_config()

        if not config.dev_server_pid:
            console.print("[yellow]No development server found.[/yellow]")
            return

        console.print("[bold yellow]Stopping development server...[/bold yellow]")

        # Try to send stop request to dev server first
        if config.dev_server_port and self._is_process_running(config.dev_server_pid):
            import requests

            try:
                response = requests.post(
                    f"http://localhost:{config.dev_server_port}/actions/stop",
                    timeout=5,
                )
                if response.status_code == 200:
                    console.print("[green]✓[/green] Stopped all servers via API")
            except Exception:
                # If API fails, we'll kill the process below
                pass

        # Kill the dev server process and all its children
        if self._is_process_running(config.dev_server_pid):
            try:
                # Get the process
                process = psutil.Process(config.dev_server_pid)

                # Get all child processes
                children = process.children(recursive=True)

                # Terminate the main process
                process.terminate()

                # Terminate all children
                for child in children:
                    try:
                        child.terminate()
                    except (psutil.NoSuchProcess, psutil.AccessDenied):
                        pass

                # Wait for process to terminate
                try:
                    process.wait(timeout=5)
                    console.print(
                        f"[green]✓[/green] Stopped dev server (PID: {config.dev_server_pid})"
                    )
                except psutil.TimeoutExpired:
                    # Force kill if it didn't terminate
                    process.kill()
                    for child in children:
                        try:
                            child.kill()
                        except (psutil.NoSuchProcess, psutil.AccessDenied):
                            pass
                    console.print(
                        f"[green]✓[/green] Force killed dev server (PID: {config.dev_server_pid})"
                    )

            except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                console.print(
                    f"[yellow]⚠️  Could not stop dev server (PID: {config.dev_server_pid}): {e}[/yellow]"
                )
        else:
            console.print(
                f"[dim]Dev server (PID: {config.dev_server_pid}) was not running[/dim]"
            )

        # Clear dev server info
        config.dev_server_pid = None
        config.dev_server_port = None
        config.write_to_file(self.project_json_path)

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
        config = self._get_or_create_config()

        if not config.dev_server_pid or not config.dev_server_port:
            console.print("[yellow]No development server found.[/yellow]")
            return

        if not self._is_process_running(config.dev_server_pid):
            console.print("[red]Development server is not running.[/red]")
            return

        # Determine process filter
        # Note: app_only is handled client-side because it's a subset of backend logs
        process_filter = "all"
        if ui_only and not backend_only and not openapi_only and not app_only:
            process_filter = "frontend"
        elif backend_only and not ui_only and not openapi_only and not app_only:
            process_filter = "backend"
        elif openapi_only and not ui_only and not backend_only and not app_only:
            process_filter = "openapi"
        elif app_only and not ui_only and not backend_only and not openapi_only:
            # For app-only, we need backend logs and will filter client-side
            process_filter = "backend"

        # Connect to SSE endpoint
        import requests
        import json

        params: dict[str, str | int] = {"process": process_filter}
        if duration_seconds:
            params["duration"] = str(duration_seconds)

        log_count = 0  # Initialize early to avoid unbound error

        try:
            response = requests.get(
                f"http://localhost:{config.dev_server_port}/logs",
                params=params,
                stream=True,
                timeout=None,
            )

            start_time = time.time()

            for line in response.iter_lines():
                # Check timeout
                if timeout_seconds and (time.time() - start_time) >= timeout_seconds:
                    if follow:
                        console.print("\n[dim]Timeout reached, stopping stream.[/dim]")
                    break

                if line:
                    line_str = line.decode("utf-8")

                    # Check for sentinel event marking end of buffered logs
                    if line_str.startswith("event: buffered_done"):
                        # If not following, we're done after buffered logs
                        if not follow:
                            break
                        # If following, continue to stream new logs
                        continue

                    if line_str.startswith("data: "):
                        # Parse SSE data
                        data_str = line_str[6:]  # Remove "data: " prefix
                        try:
                            log_entry = json.loads(data_str)

                            # Client-side filtering for app-only logs
                            if app_only:
                                # Only show backend logs that have "APP | " prefix
                                if log_entry.get("process_name") != "backend":
                                    continue
                                content = log_entry.get("content", "")
                                if not content.startswith("APP | "):
                                    continue

                            self._print_log_entry(log_entry, raw_output=raw_output)
                            log_count += 1
                        except json.JSONDecodeError:
                            pass

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

    def _print_log_entry(self, log: dict[str, Any], raw_output: bool = False):
        """Print a single log entry with formatting.

        Args:
            log: Log entry dict with keys: process_name, content, timestamp
            raw_output: If True, print raw log content without prefix formatting
        """
        content = log.get("content", "")
        timestamp = log.get("timestamp", log.get("created_at", ""))

        # For raw output, strip the internal prefix and print without color/formatting
        if raw_output:
            # For backend logs, strip the "BE | " or "APP | " prefix
            if log["process_name"] == "backend" and " | " in content:
                stream_prefix, message = content.split(" | ", 1)
                if stream_prefix in ["BE", "APP"]:
                    content = message
            # Print without rich formatting
            print(f"{content}")
            return

        # For backend logs, parse the stream prefix (BE or APP)
        if log["process_name"] == "backend":
            # Content format is "BE | message" or "APP | message"
            if " | " in content:
                stream_prefix, message = content.split(" | ", 1)
                if stream_prefix == "BE":
                    prefix_color = "green"
                    prefix = "[BE] "
                    content = message
                elif stream_prefix == "APP":
                    prefix_color = "yellow"
                    prefix = "[APP]"
                    content = message
                else:
                    # Fallback if unknown stream
                    prefix_color = "green"
                    prefix = "[BE] "
            else:
                # No stream prefix found, default to BE
                prefix_color = "green"
                prefix = "[BE] "
        elif log["process_name"] == "frontend":
            prefix_color = "cyan"
            prefix = "[UI] "
        elif log["process_name"] == "openapi":
            # OpenAPI watcher logs come directly from the logger (no stream prefix)
            prefix_color = "magenta"
            prefix = "[GEN]"
        else:
            # Default fallback
            prefix_color = "white"
            prefix = f"[{log['process_name'].upper()}]".ljust(5)

        console.print(
            f"[dim]{timestamp}[/dim] | [{prefix_color}]{prefix}[/{prefix_color}] | {content}"
        )
