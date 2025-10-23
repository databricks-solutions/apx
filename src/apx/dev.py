"""Development server utilities for apx."""

import asyncio
import contextlib
import importlib
import io
import logging
import sys
import time
from pathlib import Path

from databricks.sdk import WorkspaceClient
from dotenv import load_dotenv, set_key, get_key
from fastapi import FastAPI, Request
from starlette.middleware.base import BaseHTTPMiddleware
from typer import Exit
import watchfiles
import uvicorn
from apx.utils import console, PrefixedLogHandler
from apx import __version__


# note: header name must be lowercase and with - symbols
ACCESS_TOKEN_HEADER_NAME = "x-forwarded-access-token"


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


def create_and_persist_obo_token(
    ws: WorkspaceClient,
    app_module_name: str,
    token_lifetime_seconds: int,
    env_file: Path,
    status_context=None,
):
    # Step 3.2: Create a new token
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

    # Save token to .env file
    with suppress_output_and_logs():
        set_key(env_file, "APX_TOKEN_ID", new_token.token_info.token_id)
        set_key(env_file, "APX_TOKEN_SECRET", new_token.token_value)

    if status_context:
        status_context.update(f"💾 Token saved to {env_file.name}")

    return new_token.token_value


def prepare_obo_token(
    cwd: Path,
    app_module_name: str,
    token_lifetime_seconds: int = 60 * 60 * 4,
    status_context=None,
) -> str:
    """Prepare the On-Behalf-Of token for the backend server.

    1. Check if .env file exists, if not, create it
    2. check if .env file is gitignored, if not, - raise an error
    4. check if APX_TOKEN_ID and APX_TOKEN_SECRET are set in the .env file
        4.1 If they're set, check if they're valid and lifetime is longer than 1 hour.
            If all right, add the token value to the On-Behalf-Of header.
        4.2 If they're not set, create a new token and add it to the .env file.
        4.3 Return the token value.
    """
    env_file = cwd / ".env"
    gitignore_file = cwd / ".gitignore"

    # Step 1: Check if .env exists, create if not
    if not env_file.exists():
        if status_context:
            status_context.update("📝 Creating .env file")
        env_file.touch()

    # Step 2: Check if .env is gitignored
    if gitignore_file.exists():
        gitignore_content = gitignore_file.read_text()
        if ".env" not in gitignore_content:
            console.print(
                "[red]❌ .env file is not in .gitignore. Please add it to avoid committing secrets.[/red]"
            )
            raise Exit(code=1)
    else:
        console.print(
            "[yellow]⚠️  .gitignore not found. Please ensure .env is not committed.[/yellow]"
        )

    # pick specific env variables
    if status_context:
        status_context.update("🔍 Checking existing token")

    # Suppress output from get_key (which prints warnings) and any logging
    try:
        with suppress_output_and_logs():
            token_id = get_key(env_file, "APX_TOKEN_ID")
            token_secret = get_key(env_file, "APX_TOKEN_SECRET")
            # Initialize Databricks client
            ws = WorkspaceClient(product="apx/dev", product_version=__version__)
    except Exception as e:
        console.print(f"[red]❌ Failed to initialize Databricks client: {e}[/red]")
        console.print(
            "[yellow]💡 Make sure you have Databricks credentials configured.[/yellow]"
        )
        raise Exit(code=1)

    # Step 3: Check if token ID and secret are set
    if token_id and token_secret:
        # Step 3.1: Validate the token
        if status_context:
            status_context.update("🔐 Validating existing token")

        # Suppress any logging during token validation
        with suppress_output_and_logs():
            user_tokens = ws.tokens.list()
            user_token = next(
                (token for token in user_tokens if token.token_id == token_id), None
            )

        # Check if token exists and is still valid
        if user_token and user_token.expiry_time:
            # expiry_time is in milliseconds since epoch
            expiry_timestamp = user_token.expiry_time / 1000
            current_time = time.time()
            time_remaining = expiry_timestamp - current_time

            # Use existing token if it has at least 1 hour remaining
            min_remaining_time = 60 * 60  # 1 hour in seconds
            if time_remaining > min_remaining_time:
                if status_context:
                    status_context.update(
                        f"✅ Using existing token (expires in {int(time_remaining / 3600)} hours)"
                    )
                return token_secret

        # Token not found, expired, or no expiry time - create a new one
        if status_context:
            status_context.update("⚠️  Token expired, creating new one")

    # Create and return new token (common path for all cases that need a new token)
    return create_and_persist_obo_token(
        ws,
        app_module_name,
        token_lifetime_seconds,
        env_file,
        status_context=status_context,
    )


def setup_uvicorn_logging():
    """Configure uvicorn loggers to use PrefixedLogHandler."""
    # Create the handler
    handler = PrefixedLogHandler(prefix="[backend]", color="aquamarine1")

    # Set a simple formatter (no timestamp since we're adding prefix)
    formatter = logging.Formatter("%(message)s")
    handler.setFormatter(formatter)

    # Configure ONLY uvicorn loggers (not the root logger or app loggers)
    for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
        logger = logging.getLogger(logger_name)
        # Remove existing handlers
        logger.handlers.clear()
        # Add our custom handler
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
        # Prevent propagation to root logger
        logger.propagate = False


async def run_backend(
    cwd: Path,
    app_module_name: str,
    backend_host: str,
    backend_port: int,
    obo: bool = False,
):
    """Run the backend server programmatically with uvicorn and hot-reload support."""

    # Setup uvicorn logging once at the start
    setup_uvicorn_logging()

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
            if not first_run:
                console.print("[yellow][server][/yellow] Reloading...")
                console.print()

            # Reload .env file on every iteration (including first run)
            dotenv_file = cwd / ".env"
            if dotenv_file.exists():
                # Override=True ensures we reload env vars on hot reload
                load_dotenv(dotenv_file)

            # Prepare OBO token (will reuse if still valid)
            if obo and first_run:
                with console.status(
                    "[bold cyan]Preparing On-Behalf-Of token..."
                ) as status:
                    status.update(f"📂 Loading .env file from {dotenv_file.resolve()}")
                    obo_token = prepare_obo_token(
                        cwd, app_module_name, status_context=status
                    )
                    # Give user a moment to see the final status
                    time.sleep(0.3)
                console.print("[green]✓[/green] On-Behalf-Of token ready")
                console.print()
            elif obo:
                # On hot reload, prepare token without spinner
                obo_token = prepare_obo_token(cwd, app_module_name, status_context=None)

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

                app_instance.add_middleware(BaseHTTPMiddleware, dispatch=obo_middleware)

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
