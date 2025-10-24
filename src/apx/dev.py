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
from typing import Optional

import keyring
from databricks.sdk import WorkspaceClient
from dotenv import load_dotenv
from fastapi import FastAPI, Request
from pydantic import BaseModel, Field
from rich.table import Table
from starlette.middleware.base import BaseHTTPMiddleware
from typer import Exit
import watchfiles
import uvicorn
from apx.utils import (
    console,
    PrefixedLogHandler,
    ensure_dir,
    get_project_metadata,
    in_path,
)
from apx import __version__


# note: header name must be lowercase and with - symbols
ACCESS_TOKEN_HEADER_NAME = "x-forwarded-access-token"


# === Pydantic Models for Project Configuration ===


class ProcessInfo(BaseModel):
    """Information about a running process."""

    pid: int
    port: Optional[int] = None
    created_at: str = Field(default_factory=lambda: time.strftime("%Y-%m-%d %H:%M:%S"))


class ProjectConfig(BaseModel):
    """Configuration stored in .apx/project.json."""

    application_id: str
    token_id: Optional[str] = None
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
            f"[red]❌ Invalid app module format. Expected format: some.package.file:app[/red]"
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


def prepare_obo_token(
    cwd: Path,
    app_module_name: str,
    token_lifetime_seconds: int = 60 * 60 * 4,
    status_context=None,
    dev_manager: "DevManager | None" = None,
) -> str:
    """Prepare the On-Behalf-Of token for the backend server.

    Checks keyring and project.json for existing valid token, creates new one if needed.
    Only stores in keyring (secure) and token_id in project.json (not sensitive).
    """
    # Initialize Databricks client
    try:
        with suppress_output_and_logs():
            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    except Exception as e:
        console.print(f"[red]❌ Failed to initialize Databricks client: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
        )
        raise Exit(code=1)

    # Step 1: Check keyring for token
    if dev_manager:
        if status_context:
            status_context.update("🔍 Checking keyring for existing token")

        keyring_token = get_token_from_keyring(dev_manager.app_id)
        stored_token_id = get_token_id(cwd)

        # If we have both token and token_id, validate the token
        if keyring_token and stored_token_id:
            if status_context:
                status_context.update("🔐 Validating existing token")

            # Suppress any logging during token validation
            with suppress_output_and_logs():
                user_tokens = ws.tokens.list()
                user_token = next(
                    (
                        token
                        for token in user_tokens
                        if token.token_id == stored_token_id
                    ),
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
                status_context.update(
                    "⚠️  Token found but missing metadata, recreating..."
                )
            delete_token_from_keyring(dev_manager.app_id)

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
    if dev_manager:
        save_token_to_keyring(dev_manager.app_id, new_token)
        save_token_id(cwd, token_id)
        if status_context:
            status_context.update("💾 Token stored securely in keyring")

    return new_token


def setup_uvicorn_logging(log_file: Path | None = None):
    """Configure uvicorn loggers to use file logging or console.

    Args:
        log_file: Optional log file path for file-based logging
    """
    from logging.handlers import TimedRotatingFileHandler, MemoryHandler

    # Configure ONLY uvicorn loggers
    for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
        logger = logging.getLogger(logger_name)
        logger.handlers.clear()

        if log_file:
            # File logging with rotation
            base_handler = TimedRotatingFileHandler(
                log_file,
                when="H",
                interval=1,
                backupCount=0,
            )
            formatter = logging.Formatter("%(asctime)s | %(levelname)s | %(message)s")
            base_handler.setFormatter(formatter)

            # Add buffering to reduce I/O while maintaining low latency
            handler = MemoryHandler(
                capacity=10,  # Buffer up to 10 log records
                flushLevel=logging.ERROR,  # Flush immediately on ERROR or higher
                target=base_handler,
            )
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
):
    """Run the backend server programmatically with uvicorn and hot-reload support.

    Args:
        cwd: Current working directory
        app_module_name: Module name for the FastAPI app
        backend_host: Host to bind to
        backend_port: Port to bind to
        obo: Whether to enable On-Behalf-Of token middleware
        log_file: Optional log file path for file-based logging
    """

    # Setup uvicorn logging once at the start
    setup_uvicorn_logging(log_file)

    if not log_file:
        console.print(
            f"[green][server][/]Starting server on {backend_host}:{backend_port} from app: {app_module_name}"
        )
        console.print(f"[green][server][/]Watching for changes in {cwd}/**/*.py")
        console.print()

    # Track if this is the first run
    first_run = True

    # Store OBO token for reuse
    obo_token = None

    # Start periodic flush task if logging to file
    flush_task = None
    if log_file:

        async def periodic_flush():
            """Periodically flush logger buffers to ensure ~100ms latency."""
            while True:
                await asyncio.sleep(0.1)  # Flush every 100ms
                for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
                    logger = logging.getLogger(logger_name)
                    for handler in logger.handlers:
                        handler.flush()

        flush_task = asyncio.create_task(periodic_flush())

    try:
        while True:
            server = None
            server_task = None
            watch_task = None

            try:
                # Reload message
                if not first_run and not log_file:
                    console.print("[yellow][server][/yellow] Reloading...")
                    console.print()

                # Reload .env file on every iteration (including first run)
                dotenv_file = cwd / ".env"
                if dotenv_file.exists():
                    # Override=True ensures we reload env vars on hot reload
                    load_dotenv(dotenv_file)

                # Prepare OBO token (will reuse if still valid)
                if obo and first_run:
                    if log_file:
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
                        if not log_file:
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
    finally:
        # Clean up flush task if it exists
        if flush_task:
            flush_task.cancel()
            try:
                await flush_task
            except asyncio.CancelledError:
                pass
            # Final flush
            for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
                logger = logging.getLogger(logger_name)
                for handler in logger.handlers:
                    handler.flush()


# === File-based Logging Utilities ===


def get_or_create_app_id(app_dir: Path) -> str:
    """Get or create application ID for this project.

    Args:
        app_dir: Application directory

    Returns:
        Application ID (UUID string)
    """
    import uuid

    project_json_path = app_dir / ".apx" / "project.json"
    ensure_dir(app_dir / ".apx")

    if project_json_path.exists():
        try:
            config = ProjectConfig.read_from_file(project_json_path)
            return config.application_id
        except Exception:
            pass

    # Generate new ID and create new config
    app_id = str(uuid.uuid4())
    config = ProjectConfig(application_id=app_id)
    config.write_to_file(project_json_path)
    return app_id


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
        import uuid

        config = ProjectConfig(application_id=str(uuid.uuid4()))

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


def save_token_to_keyring(app_id: str, token_value: str):
    """Save token to system keyring.

    Args:
        app_id: Application ID (used as keyring key)
        token_value: Token value to store
    """
    keyring.set_password("apx-dev", app_id, token_value)


def get_token_from_keyring(app_id: str) -> str | None:
    """Get token from system keyring.

    Args:
        app_id: Application ID (used as keyring key)

    Returns:
        Token value or None if not found
    """
    return keyring.get_password("apx-dev", app_id)


def delete_token_from_keyring(app_id: str):
    """Delete token from system keyring.

    Args:
        app_id: Application ID (used as keyring key)
    """
    try:
        keyring.delete_password("apx-dev", app_id)
    except Exception:
        # Password might not exist, that's fine
        pass


def get_log_dir(app_id: str) -> Path:
    """Get the log directory for an application.

    Args:
        app_id: Application ID

    Returns:
        Path to log directory (~/.apx/{app_id}/)
    """
    log_dir = Path.home() / ".apx" / app_id
    ensure_dir(log_dir)
    return log_dir


def setup_file_logger(log_file: Path, process_name: str) -> logging.Logger:
    """Setup a rotating file logger that deletes logs after 1 hour.

    Args:
        log_file: Path to log file
        process_name: Name of the process (for logger name)

    Returns:
        Configured logger instance
    """
    from logging.handlers import TimedRotatingFileHandler

    logger = logging.getLogger(f"apx.{process_name}")
    logger.setLevel(logging.INFO)
    logger.handlers.clear()

    # Create handler that rotates every hour and keeps only 1 backup
    handler = TimedRotatingFileHandler(
        log_file,
        when="H",  # Rotate every hour
        interval=1,
        backupCount=0,  # Don't keep backups (effectively deletes after 1 hour)
        encoding="utf-8",
    )

    # Add buffering to reduce I/O while maintaining low latency
    # Flush every 10 records to provide ~100ms latency without overloading I/O
    from logging.handlers import MemoryHandler

    memory_handler = MemoryHandler(
        capacity=10,  # Buffer up to 10 log records for balanced latency/performance
        flushLevel=logging.ERROR,  # Flush immediately on ERROR or higher
        target=handler,
    )

    # Format: timestamp | stream | message
    formatter = logging.Formatter("%(asctime)s | %(levelname)s | %(message)s")
    handler.setFormatter(formatter)

    logger.addHandler(memory_handler)
    logger.propagate = False

    return logger


async def run_frontend_with_logging(app_dir: Path, app_id: str, port: int):
    """Run frontend dev server and capture output to log file.

    Args:
        app_dir: Application directory
        app_id: Application ID
        port: Frontend port
    """
    log_dir = get_log_dir(app_id)
    log_file = log_dir / "frontend.log"

    logger = setup_file_logger(log_file, "frontend")

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

    async def periodic_flush():
        """Periodically flush logger buffers to ensure ~100ms latency."""
        while True:
            await asyncio.sleep(0.1)  # Flush every 100ms
            for handler in logger.handlers:
                handler.flush()

    # Start periodic flush task
    flush_task = asyncio.create_task(periodic_flush())

    try:
        # Read both stdout and stderr
        await asyncio.gather(
            read_stream(process.stdout, "stdout"),
            read_stream(process.stderr, "stderr"),
        )

        await process.wait()
    finally:
        # Clean up flush task
        flush_task.cancel()
        try:
            await flush_task
        except asyncio.CancelledError:
            pass
        # Final flush
        for handler in logger.handlers:
            handler.flush()


async def run_openapi_with_logging(app_dir: Path, app_id: str):
    """Run OpenAPI watcher and capture output to log file.

    Args:
        app_dir: Application directory
        app_id: Application ID
    """
    from apx.openapi import _openapi_watch

    log_dir = get_log_dir(app_id)
    log_file = log_dir / "openapi.log"

    logger = setup_file_logger(log_file, "openapi")

    # Redirect console output to logger
    import sys

    class LoggerWriter:
        def __init__(self, logger, level):
            self.logger = logger
            self.level = level
            self.buffer = []

        def write(self, message):
            if message and message.strip():
                self.logger.log(self.level, f"stdout | {message.strip()}")

        def flush(self):
            # Flush the logger handlers
            for handler in self.logger.handlers:
                handler.flush()

    async def periodic_flush():
        """Periodically flush logger buffers to ensure ~100ms latency."""
        while True:
            await asyncio.sleep(0.1)  # Flush every 100ms
            for handler in logger.handlers:
                handler.flush()

    # Capture stdout/stderr
    old_stdout = sys.stdout
    old_stderr = sys.stderr

    # Start periodic flush task
    flush_task = asyncio.create_task(periodic_flush())

    try:
        sys.stdout = LoggerWriter(logger, logging.INFO)
        sys.stderr = LoggerWriter(logger, logging.ERROR)

        # Run the OpenAPI watcher
        await _openapi_watch(app_dir)
    finally:
        sys.stdout = old_stdout
        sys.stderr = old_stderr
        # Clean up flush task
        flush_task.cancel()
        try:
            await flush_task
        except asyncio.CancelledError:
            pass
        # Final flush
        for handler in logger.handlers:
            handler.flush()


# === DevManager Class ===


class DevManager:
    """Manages development server processes with file-based logging."""

    def __init__(self, app_dir: Path):
        """Initialize the DevManager with an app directory.

        Args:
            app_dir: The path to the application directory
        """
        self.app_dir = app_dir
        self.apx_dir = app_dir / ".apx"
        self.project_json_path = self.apx_dir / "project.json"
        self.app_id = get_or_create_app_id(app_dir)
        self.log_dir = get_log_dir(self.app_id)

    def _get_or_create_config(self) -> ProjectConfig:
        """Get or create project configuration."""
        ensure_dir(self.apx_dir)

        if self.project_json_path.exists():
            try:
                return ProjectConfig.read_from_file(self.project_json_path)
            except Exception:
                pass

        # Create new config
        config = ProjectConfig(application_id=self.app_id)
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
    ):
        """Start development servers in detached mode.

        Args:
            frontend_port: Port for the frontend development server
            backend_port: Port for the backend server
            backend_host: Host for the backend server
            obo: Whether to add On-Behalf-Of header to the backend server
            openapi: Whether to start OpenAPI watcher process
        """
        # Check if servers are already running
        existing_processes = self._get_all_processes()
        for name, pid, _ in existing_processes:
            if self._is_process_running(pid):
                console.print(
                    f"[yellow]⚠️  Dev server is already running (PID: {pid}). Run 'apx dev stop' first.[/yellow]"
                )
                raise Exit(code=1)
            else:
                # Clean up stale entries
                self._remove_process_pid(name)

        with in_path(self.app_dir):
            # Get app module name from pyproject.toml
            app_module_name = get_project_metadata()["app-module"]

            console.print(
                f"[bold chartreuse1]🚀 Starting development servers in detached mode...[/bold chartreuse1]"
            )
            console.print(f"[cyan]Frontend:[/cyan] http://localhost:{frontend_port}")
            console.print(
                f"[green]Backend:[/green] http://{backend_host}:{backend_port}"
            )
            console.print()

            # Start frontend process using internal command
            frontend_proc = subprocess.Popen(
                [
                    "uv",
                    "run",
                    "apx",
                    "_run-frontend-detached",
                    str(self.app_dir),
                    self.app_id,
                    str(frontend_port),
                ],
                cwd=self.app_dir,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )

            self._save_process_pid("frontend", frontend_proc.pid, frontend_port)
            console.print(
                f"[cyan]✓[/cyan] Frontend process started (PID: {frontend_proc.pid})"
            )
            console.print(f"[dim]  Logs: {self.log_dir / 'frontend.log'}[/dim]")

            # Start backend process using internal command
            backend_proc = subprocess.Popen(
                [
                    "uv",
                    "run",
                    "apx",
                    "_run-backend-detached",
                    str(self.app_dir),
                    self.app_id,
                    app_module_name,
                    backend_host,
                    str(backend_port),
                    str(obo).lower(),
                ],
                cwd=self.app_dir,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )

            self._save_process_pid("backend", backend_proc.pid, backend_port)
            console.print(
                f"[green]✓[/green] Backend process started (PID: {backend_proc.pid})"
            )
            console.print(f"[dim]  Logs: {self.log_dir / 'backend.log'}[/dim]")

            # Start OpenAPI watcher process if enabled
            if openapi:
                openapi_proc = subprocess.Popen(
                    [
                        "uv",
                        "run",
                        "apx",
                        "_run-openapi-detached",
                        str(self.app_dir),
                        self.app_id,
                    ],
                    cwd=self.app_dir,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    start_new_session=True,
                )

                self._save_process_pid("openapi", openapi_proc.pid, None)
                console.print(
                    f"[magenta]✓[/magenta] OpenAPI watcher started (PID: {openapi_proc.pid})"
                )
                console.print(f"[dim]  Logs: {self.log_dir / 'openapi.log'}[/dim]")

            console.print()
            console.print(
                "[bold green]✨ Development servers started successfully![/bold green]"
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

        processes = self._get_all_processes()

        if not processes:
            console.print("[yellow]No development servers found.[/yellow]")
            console.print("[dim]Run 'apx dev start' to start the servers.[/dim]")
            return

        # Create a status table
        table = Table(
            title="Development Server Status",
            show_header=True,
            header_style="bold magenta",
        )
        table.add_column("Process", style="cyan", width=12)
        table.add_column("PID", justify="right", style="blue")
        table.add_column("Port", justify="right", style="green")
        table.add_column("Status", justify="center")

        for name, pid, port in processes:
            is_running = self._is_process_running(pid)
            status = (
                "[green]●[/green] Running" if is_running else "[red]●[/red] Stopped"
            )
            port_str = str(port) if port else "-"
            table.add_row(name, str(pid), port_str, status)

        console.print(table)
        console.print()
        console.print(f"[dim]Logs: {self.log_dir}[/dim]")
        console.print("[dim]Use 'apx dev logs' or 'apx dev tail' to view.[/dim]")

    def stop(self):
        """Stop development servers."""
        if not self.project_json_path.exists():
            console.print("[yellow]No development servers found.[/yellow]")
            return

        processes = self._get_all_processes()

        if not processes:
            console.print("[yellow]No development servers found.[/yellow]")
            return

        console.print("[bold yellow]Stopping development servers...[/bold yellow]")

        for name, pid, _ in processes:
            if self._is_process_running(pid):
                try:
                    # Get the process
                    process = psutil.Process(pid)

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
                        console.print(f"[green]✓[/green] Stopped {name} (PID: {pid})")
                    except psutil.TimeoutExpired:
                        # Force kill if it didn't terminate
                        process.kill()
                        for child in children:
                            try:
                                child.kill()
                            except (psutil.NoSuchProcess, psutil.AccessDenied):
                                pass
                        console.print(
                            f"[green]✓[/green] Force killed {name} (PID: {pid})"
                        )

                except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                    console.print(
                        f"[yellow]⚠️  Could not stop {name} (PID: {pid}): {e}[/yellow]"
                    )
            else:
                console.print(f"[dim]  {name} (PID: {pid}) was not running[/dim]")

            # Remove from database
            self._remove_process_pid(name)

        console.print()
        console.print(
            "[bold green]✨ Development servers stopped successfully![/bold green]"
        )
        console.print("[dim]  Token remains valid in keyring until expiration[/dim]")

    def get_logs(
        self,
        duration_seconds: int | None = None,
        ui_only: bool = False,
        backend_only: bool = False,
        openapi_only: bool = False,
        limit: int = 1000,
    ) -> list[dict]:
        """Retrieve logs from log files with filtering.

        Args:
            duration_seconds: Show logs from last N seconds (None = all logs)
            ui_only: Only show frontend logs
            backend_only: Only show backend logs
            openapi_only: Only show OpenAPI logs
            limit: Maximum number of log entries to return

        Returns:
            List of log entries sorted by timestamp (oldest first)
        """
        import datetime

        results = []
        cutoff_time = None

        if duration_seconds:
            cutoff_time = datetime.datetime.now() - datetime.timedelta(
                seconds=duration_seconds
            )

        # Determine which log files to read
        log_files = []
        filters_active = ui_only or backend_only or openapi_only

        if ui_only and not backend_only and not openapi_only:
            log_files = [("frontend", self.log_dir / "frontend.log")]
        elif backend_only and not ui_only and not openapi_only:
            log_files = [("backend", self.log_dir / "backend.log")]
        elif openapi_only and not ui_only and not backend_only:
            log_files = [("openapi", self.log_dir / "openapi.log")]
        else:
            # Show all logs if no filter or multiple filters
            log_files = [
                ("frontend", self.log_dir / "frontend.log"),
                ("backend", self.log_dir / "backend.log"),
                ("openapi", self.log_dir / "openapi.log"),
            ]

        for process_name, log_file in log_files:
            if not log_file.exists():
                continue

            try:
                with open(log_file, "r") as f:
                    for line in f:
                        line = line.strip()
                        if not line:
                            continue

                        # Parse log line: timestamp | level | stream | message
                        parts = line.split(" | ", 2)
                        if len(parts) < 3:
                            continue

                        timestamp_str, level, content = parts

                        # Filter by time if needed
                        if cutoff_time:
                            try:
                                log_time = datetime.datetime.strptime(
                                    timestamp_str, "%Y-%m-%d %H:%M:%S,%f"
                                )
                                if log_time < cutoff_time:
                                    continue
                            except Exception:
                                pass

                        results.append(
                            {
                                "process_name": process_name,
                                "content": content,
                                "created_at": timestamp_str,
                                "level": level,
                            }
                        )
            except Exception:
                pass

        # Sort by timestamp and limit
        results.sort(key=lambda x: x["created_at"])
        return results[:limit]

    def tail_logs(
        self,
        duration_seconds: int | None = None,
        ui_only: bool = False,
        backend_only: bool = False,
        openapi_only: bool = False,
        timeout_seconds: int | None = None,
    ):
        """Tail logs continuously from log files.

        Args:
            duration_seconds: Initially show logs from last N seconds
            ui_only: Only show frontend logs
            backend_only: Only show backend logs
            openapi_only: Only show OpenAPI logs
            timeout_seconds: Stop tailing after N seconds (None = indefinite)
        """
        # Get initial logs
        initial_logs = self.get_logs(
            duration_seconds=duration_seconds,
            ui_only=ui_only,
            backend_only=backend_only,
            openapi_only=openapi_only,
            limit=1000,
        )

        # Display initial logs
        for log in initial_logs:
            self._print_log_entry(log)

        # Determine which log files to tail
        log_files = []
        if ui_only and not backend_only and not openapi_only:
            log_files = [("frontend", self.log_dir / "frontend.log")]
        elif backend_only and not ui_only and not openapi_only:
            log_files = [("backend", self.log_dir / "backend.log")]
        elif openapi_only and not ui_only and not backend_only:
            log_files = [("openapi", self.log_dir / "openapi.log")]
        else:
            log_files = [
                ("frontend", self.log_dir / "frontend.log"),
                ("backend", self.log_dir / "backend.log"),
                ("openapi", self.log_dir / "openapi.log"),
            ]

        # Open files and seek to end
        file_handles = {}
        for process_name, log_file in log_files:
            if log_file.exists():
                f = open(log_file, "r")
                f.seek(0, 2)  # Seek to end
                file_handles[process_name] = f

        start_time = time.time()

        try:
            while True:
                # Check timeout
                if timeout_seconds and (time.time() - start_time) >= timeout_seconds:
                    console.print("\n[dim]Timeout reached, stopping tail.[/dim]")
                    break

                # Check each file for new lines
                for process_name, f in file_handles.items():
                    line = f.readline()
                    if line:
                        line = line.strip()
                        if line:
                            # Parse log line
                            parts = line.split(" | ", 2)
                            if len(parts) >= 3:
                                timestamp_str, level, content = parts
                                log_entry = {
                                    "process_name": process_name,
                                    "content": content,
                                    "created_at": timestamp_str,
                                    "level": level,
                                }
                                self._print_log_entry(log_entry)

                # Sleep briefly before checking again
                time.sleep(0.5)

        except KeyboardInterrupt:
            console.print("\n[dim]Stopped tailing logs.[/dim]")
        finally:
            # Close file handles
            for f in file_handles.values():
                f.close()

    def _print_log_entry(self, log: dict):
        """Print a single log entry with formatting.

        Args:
            log: Log entry dict with keys: id, process_name, content, created_at
        """
        # Color code by process
        if log["process_name"] == "frontend":
            prefix_color = "cyan"
            prefix = "[UI]"
        elif log["process_name"] == "openapi":
            prefix_color = "magenta"
            prefix = "[API]"
        else:
            prefix_color = "green"
            prefix = "[BE]"

        # Format timestamp (show only time part for brevity)
        timestamp = (
            log["created_at"].split()[1]
            if " " in log["created_at"]
            else log["created_at"]
        )

        console.print(
            f"[dim]{timestamp}[/dim] [{prefix_color}]{prefix}[/{prefix_color}] {log['content']}"
        )
