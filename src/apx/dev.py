"""Development server utilities for apx."""

import asyncio
import importlib
import sys
from pathlib import Path

from fastapi import FastAPI, Request
from starlette.middleware.base import BaseHTTPMiddleware
from rich.console import Console
from typer import Exit
import watchfiles
import uvicorn

console = Console()
ACCESS_TOKEN_HEADER_NAME = "X-Forwarded-Access-Token"


class DevAccessTokenMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        request.scope["headers"].append((ACCESS_TOKEN_HEADER_NAME.encode(), b"true"))
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

    # Add the twist middleware
    app_instance.add_middleware(DevAccessTokenMiddleware)

    return app_instance


async def run_backend(
    cwd: Path, app_module_name: str, backend_host: str, backend_port: int
):
    """Run the backend server programmatically with uvicorn and hot-reload support."""

    console.print(
        f"[green][server][/green] Starting server on {backend_host}:{backend_port}"
    )
    console.print(f"[green][server][/green] Watching for changes in {cwd}/**/*.py")

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

            config = uvicorn.Config(
                app=app_instance,
                host=backend_host,
                port=backend_port,
                log_level="info",
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
