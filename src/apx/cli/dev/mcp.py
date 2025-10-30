"""MCP server implementation for apx dev commands."""

import asyncio
from pathlib import Path

from mcp.server.fastmcp import FastMCP

from apx.cli.dev.manager import DevManager
from apx.cli.dev.client import DevServerClient
from apx.cli.dev.models import (
    McpActionResponse,
    McpErrorResponse,
    McpMetadataResponse,
    McpStatusResponse,
)
from apx.utils import get_project_metadata
from apx import __version__ as apx_version

# Initialize the MCP server
mcp = FastMCP("APX Dev Server")


def _get_manager() -> DevManager:
    """Get DevManager instance for the current project directory."""
    return DevManager(Path.cwd())


def _get_dev_server_client() -> DevServerClient | None:
    """Get DevServerClient if dev server is running, None otherwise."""
    manager = _get_manager()
    config = manager.get_or_create_config()

    if not config.dev.pid or not config.dev.port:
        return None

    if not manager._is_process_running(config.dev.pid):
        return None

    return DevServerClient(f"http://localhost:{config.dev.port}")


@mcp.tool()
async def start(
    frontend_port: int = 5173,
    backend_port: int = 8000,
    host: str = "localhost",
    obo: bool = True,
    openapi: bool = True,
    max_retries: int = 10,
) -> McpActionResponse:
    """Start development servers (frontend, backend, and optionally OpenAPI watcher).

    Args:
        frontend_port: Port for the frontend development server (default: 5173)
        backend_port: Port for the backend server (default: 8000)
        host: Host for dev, frontend, and backend servers (default: localhost)
        obo: Whether to add On-Behalf-Of header to the backend server (default: True)
        This enables OBO token generation for Databricks API calls
        openapi: Whether to start OpenAPI watcher process (default: True)
        max_retries: Maximum number of retry attempts for processes (default: 10)

    Returns:
        McpActionResponse with status and message indicating success or failure
    """
    manager = _get_manager()

    try:
        # Run sync operation in thread pool
        await asyncio.to_thread(
            manager.start,
            frontend_port=frontend_port,
            backend_port=backend_port,
            host=host,
            obo=obo,
            openapi=openapi,
            max_retries=max_retries,
            watch=False,  # MCP tools always run in detached mode
        )
        return McpActionResponse(
            status="success", message="Development servers started successfully"
        )
    except Exception as e:
        return McpActionResponse(
            status="error", message=f"Failed to start servers: {str(e)}"
        )


@mcp.tool()
async def restart() -> McpActionResponse:
    """Restart development servers.

    This will restart all running development servers (frontend, backend, OpenAPI watcher)
    using the same configuration they were started with.

    Returns:
        McpActionResponse with status and message indicating success or failure
    """
    manager = _get_manager()
    config = await asyncio.to_thread(manager.get_or_create_config)

    if not config.dev.pid or not config.dev.port:
        return McpActionResponse(
            status="error", message="No development server found. Run 'start' first."
        )

    is_running = await asyncio.to_thread(manager._is_process_running, config.dev.pid)
    if not is_running:
        return McpActionResponse(
            status="error",
            message="Development server is not running. Run 'start' first.",
        )

    client = DevServerClient(f"http://localhost:{config.dev.port}", timeout=10.0)

    try:
        response = await asyncio.to_thread(client.restart)
        if response.status == "success":
            return McpActionResponse(
                status="success", message="Development servers restarted successfully"
            )
        else:
            return McpActionResponse(status="error", message=response.message)
    except Exception as e:
        return McpActionResponse(
            status="error", message=f"Failed to restart servers: {str(e)}"
        )


@mcp.tool()
async def stop() -> McpActionResponse:
    """Stop all development servers.

    This will stop the frontend, backend, OpenAPI watcher, and dev server processes.

    Returns:
        McpActionResponse with status and message indicating success or failure
    """
    manager = _get_manager()

    try:
        await asyncio.to_thread(manager.stop)
        return McpActionResponse(
            status="success", message="Development servers stopped successfully"
        )
    except Exception as e:
        return McpActionResponse(
            status="error", message=f"Failed to stop servers: {str(e)}"
        )


@mcp.tool()
async def status() -> McpStatusResponse:
    """Get the status of development servers.

    Returns information about whether the frontend, backend, OpenAPI watcher,
    and dev server are running, along with their ports.

    Returns:
        McpStatusResponse with status information including:
        - dev_server_running: Whether the dev server is running
        - dev_server_port: Port of the dev server (if running)
        - dev_server_pid: PID of the dev server (if running)
        - frontend_running: Whether the frontend server is running
        - frontend_port: Port of the frontend server (if running)
        - backend_running: Whether the backend server is running
        - backend_port: Port of the backend server (if running)
        - openapi_running: Whether the OpenAPI watcher is running
    """
    manager = _get_manager()
    config = await asyncio.to_thread(manager.get_or_create_config)

    # Initialize with default values
    result = McpStatusResponse(
        dev_server_running=False,
        dev_server_port=None,
        dev_server_pid=None,
        frontend_running=False,
        frontend_port=None,
        backend_running=False,
        backend_port=None,
        openapi_running=False,
    )

    if not config.dev.pid or not config.dev.port:
        return result

    is_running = await asyncio.to_thread(manager._is_process_running, config.dev.pid)
    if not is_running:
        return result

    result.dev_server_running = True
    result.dev_server_port = config.dev.port
    result.dev_server_pid = config.dev.pid

    # Try to get status from dev server
    client = DevServerClient(f"http://localhost:{config.dev.port}")
    try:
        status_data = await asyncio.to_thread(client.status)
        result.frontend_running = status_data.frontend_running
        result.frontend_port = status_data.frontend_port
        result.backend_running = status_data.backend_running
        result.backend_port = status_data.backend_port
        result.openapi_running = status_data.openapi_running
    except Exception:
        # Dev server is running but not responding - likely still starting
        pass

    return result


@mcp.tool()
async def get_metadata() -> McpMetadataResponse | McpErrorResponse:
    """Get project metadata from pyproject.toml.

    Returns the app name, app module, and app slug as defined in the project's
    pyproject.toml file under [tool.apx.metadata].

    Returns:
        McpMetadataResponse with metadata including:
        - app_name: The user-facing app name
        - app_module: The internal app module name (Python package name)
        - app_slug: The internal app slug (URL-friendly identifier)
        - apx_version: The version of apx being used
        Or McpErrorResponse if metadata retrieval fails
    """
    try:
        metadata = await asyncio.to_thread(get_project_metadata)
        return McpMetadataResponse(
            app_name=metadata.app_name,
            app_module=metadata.app_module,
            app_slug=metadata.app_slug,
            apx_version=apx_version,
        )
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get metadata: {str(e)}")


def run_mcp_server() -> None:
    """Run the MCP server using stdio transport."""
    # FastMCP.run() automatically uses stdio when called without arguments
    mcp.run()
