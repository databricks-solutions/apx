"""Development server utilities for apx."""

import asyncio
import importlib
import logging
import os
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

ACCESS_TOKEN_HEADER_NAME = "X-Forwarded-Access-Token"


class DevAccessTokenMiddleware(BaseHTTPMiddleware):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        if "token" in kwargs:
            self.token = kwargs["token"]
        else:
            console.print(
                "[yellow]No token provided for On-Behalf-Of middleware[/yellow]"
            )

    async def dispatch(self, request: Request, call_next):
        request.scope["headers"].append(
            (ACCESS_TOKEN_HEADER_NAME.encode(), self.token.encode())
        )
        response = await call_next(request)
        return response


def load_app(app_module_name: str) -> FastAPI:
    """Load and return the FastAPI app instance."""
    # Split the app_name into module path and attribute name
    if ":" not in app_module_name:
        console.print(
            f"[red]❌ Invalid app module format. Expected format: some.package.file:app[/red]"
        )
        raise Exit(code=1)

    module_path, attribute_name = app_module_name.split(":", 1)

    # Reload the module if it's already loaded
    if module_path in sys.modules:
        importlib.reload(sys.modules[module_path])
        module = sys.modules[module_path]
    else:
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


def prepare_obo_token(
    cwd: Path, app_module_name: str, token_lifetime_seconds: int = 60 * 60 * 4
) -> str | None:
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
        console.print("[yellow][obo][/yellow] Creating .env file")
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
    token_id = get_key(env_file, "APX_TOKEN_ID")
    token_secret = get_key(env_file, "APX_TOKEN_SECRET")

    # Initialize Databricks client
    try:
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
        user_tokens = ws.tokens.list()
        user_token = next(
            (token for token in user_tokens if token.token_id == token_id), None
        )
        if not user_token:
            console.print("[yellow][obo][/yellow] Token not found, creating a new one")
            return None

        if user_token.expiry_time:
            # expiry_time is in milliseconds since epoch
            expiry_timestamp = user_token.expiry_time / 1000
            current_time = time.time()
            time_remaining = expiry_timestamp - current_time

            if time_remaining > token_lifetime_seconds:
                console.print(
                    f"[green][obo][/green] Using existing token (expires in {int(time_remaining / 3600)} hours)"
                )
                return token_secret
            else:
                console.print(
                    "[yellow][obo][/yellow] Token expires soon, creating a new one"
                )

    # Step 3.2: Create a new token
    console.print("[green][obo][/green] Creating new OBO token")
    new_token = ws.tokens.create(
        comment=f"dev token for {app_module_name}, created by apx",
        lifetime_seconds=token_lifetime_seconds,
    )

    assert new_token.token_info is not None
    assert new_token.token_info.token_id is not None
    assert new_token.token_value is not None

    # Save token to .env file
    set_key(env_file, "APX_TOKEN_ID", new_token.token_info.token_id)
    set_key(env_file, "APX_TOKEN_SECRET", new_token.token_value)

    console.print(
        f"[green][obo][/green] Token created and saved to {env_file.resolve()}"
    )

    return new_token.token_value


def setup_uvicorn_logging():
    """Configure uvicorn to use PrefixedLogHandler for all logs."""
    # Create the handler
    handler = PrefixedLogHandler(prefix="[backend]", color="aquamarine1")

    # Set a simple formatter (no timestamp since we're adding prefix)
    formatter = logging.Formatter("%(message)s")
    handler.setFormatter(formatter)

    # Configure uvicorn loggers
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

    # Setup logging once
    setup_uvicorn_logging()

    console.print(
        f"[green][server][/]Starting server on {backend_host}:{backend_port} from app: {app_module_name}"
    )
    console.print(f"[green][server][/]Watching for changes in {cwd}/**/*.py")
    console.print()

    # Track if this is the first run
    first_run = True

    while True:
        server = None
        server_task = None
        watch_task = None

        try:
            # Load the app
            if not first_run:
                console.print("[yellow][server][/yellow] Reloading...")

            app_instance = load_app(app_module_name)

            # check if .env file exists, and if yes - load it
            dotenv_file = cwd / ".env"
            if dotenv_file.exists():
                console.print(
                    f"[green][server][/green] Loading .env file from {dotenv_file.resolve()}"
                )
                load_dotenv(dotenv_file)

            if obo:
                console.print("[green][server][/green] Adding On-Behalf-Of middleware")
                app_instance.add_middleware(
                    DevAccessTokenMiddleware,
                    token=prepare_obo_token(cwd, app_module_name),
                )
                console.print(
                    f"[green][server][/green] On-Behalf-Of token was created and saved to .env"
                )
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
