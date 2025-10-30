"""MCP server implementation for apx dev commands."""

import asyncio
from pathlib import Path
import time

from mcp.server.fastmcp import FastMCP

from apx.cli.dev.manager import DevManager
from apx.cli.dev.logging import suppress_output_and_logs
from apx.cli.dev.client import DevServerClient
from apx.cli.dev.models import (
    McpActionResponse,
    McpErrorResponse,
    McpMetadataResponse,
    McpStatusResponse,
    McpUrlResponse,
)
from apx.utils import get_project_metadata
from apx import __version__ as apx_version

# Initialize the MCP server
mcp = FastMCP("APX Dev Server")


@mcp.resource("apx://info")
async def apx_info() -> str:
    """Information about apx toolkit.

    apx is a toolkit for building Databricks Apps. It provides a convenient,
    fast and AI-friendly development experience for creating full-stack applications
    with Python/FastAPI backend and React/shadcn/ui frontend.

    Key features:
    - Full-stack app development (Python + FastAPI + React + TypeScript)
    - Development server management (frontend, backend, OpenAPI watcher)
    - Integrated build and deployment tools
    - AI-friendly project structure and tooling
    - Automatic client code generation from OpenAPI schema

    This MCP server provides tools to manage development servers and access project metadata.
    """
    return """# apx - Toolkit for Building Databricks Apps

🚀 **apx** is the toolkit for building Databricks Apps ⚡**

apx bundles together a set of tools and libraries to help you with the complete app development lifecycle: develop, build and deploy.

## Overview

The main idea of apx is to provide convenient, fast and AI-friendly development experience for building modern full-stack applications.

## Technology Stack

- **Backend**: Python + FastAPI + Pydantic
- **Frontend**: React + TypeScript + shadcn/ui
- **Build Tools**: uv (Python), bun (JavaScript/TypeScript)
- **Code Generation**: orval (OpenAPI client generation)

## What This MCP Server Provides

This MCP server gives you access to development server management tools:
- **start**: Start development servers (frontend, backend, OpenAPI watcher)
- **restart**: Restart all development servers
- **stop**: Stop all development servers  
- **status**: Get status of all development servers
- **get_metadata**: Get project metadata from pyproject.toml
- **get_frontend_url**: Get the frontend development server URL

Use these tools to interact with your apx project during development."""


def _get_manager() -> DevManager:
    """Get DevManager instance for the current project directory."""
    return DevManager(Path.cwd())


def _get_dev_server_client() -> DevServerClient | None:
    """Get DevServerClient if dev server is running, None otherwise."""
    manager = _get_manager()

    if not manager.is_dev_server_running():
        return None

    return DevServerClient(manager.socket_path)


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

    def start_suppressed():
        """Start servers with suppressed console output."""
        with suppress_output_and_logs():
            manager.start(
                frontend_port=frontend_port,
                backend_port=backend_port,
                host=host,
                obo=obo,
                openapi=openapi,
                max_retries=max_retries,
                watch=False,  # MCP tools always run in detached mode
            )

    try:
        # Run sync operation in thread pool with suppressed output
        await asyncio.to_thread(start_suppressed)
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

    is_running = await asyncio.to_thread(manager.is_dev_server_running)
    if not is_running:
        return McpActionResponse(
            status="error",
            message="Development server is not running. Run 'start' first.",
        )

    def restart_suppressed():
        """Restart servers with suppressed console output."""
        with suppress_output_and_logs():
            manager.stop()
            time.sleep(1)
            manager.start()

    try:
        # Run sync operation in thread pool with suppressed output
        await asyncio.to_thread(restart_suppressed)
        return McpActionResponse(
            status="success", message="Development servers restarted successfully"
        )
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

    def stop_suppressed():
        """Stop servers with suppressed console output."""
        with suppress_output_and_logs():
            manager.stop()

    try:
        # Run sync operation in thread pool with suppressed output
        await asyncio.to_thread(stop_suppressed)
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

    is_running = await asyncio.to_thread(manager.is_dev_server_running)
    if not is_running:
        return result

    result.dev_server_running = True
    # Port and PID are no longer tracked, set to None
    result.dev_server_port = None
    result.dev_server_pid = None

    # Try to get status from dev server
    client = DevServerClient(manager.socket_path)
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
async def get_frontend_url() -> McpUrlResponse | McpErrorResponse:
    """Get the URL of the frontend development server.

    Returns:
        McpUrlResponse with the URL of the frontend development server
    """

    try:
        manager = _get_manager()
        is_running = await asyncio.to_thread(manager.is_dev_server_running)

        if not is_running:
            return McpErrorResponse(error="Dev server is not running")

        # Get frontend port from dev server status
        client = DevServerClient(manager.socket_path)
        status_data = await asyncio.to_thread(client.status)

        return McpUrlResponse(url=f"http://localhost:{status_data.frontend_port}")
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get frontend URL: {str(e)}")


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
