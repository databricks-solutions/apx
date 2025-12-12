"""MCP server implementation for apx dev commands."""

import asyncio
from pathlib import Path
import subprocess
import time
from typing import Literal, cast

from mcp.server.fastmcp import FastMCP
from pydantic import TypeAdapter
from pydantic import JsonValue

from apx.cli.dev.manager import DevManager
from apx.cli.dev.logging import suppress_output_and_logs
from apx.cli.dev.client import DevServerClient
from apx.cli.dev.models import (
    CheckCommandResult,
    JsonObject,
    McpActionResponse,
    McpDevCheckResponse,
    McpErrorResponse,
    McpMetadataResponse,
    McpOpenApiSchemaResponse,
    McpRouteCallResponse,
    McpRoutesResponse,
    McpStatusResponse,
    McpUrlResponse,
    PortsResponse,
    RouteInfo,
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


def _truncate(s: str, max_chars: int) -> str:
    if max_chars <= 0:
        return ""
    if len(s) <= max_chars:
        return s
    head = s[: max_chars - 50]
    tail = s[-40:] if max_chars >= 100 else ""
    return (
        f"{head}\n\n...[truncated {len(s) - len(head) - len(tail)} chars]...\n\n{tail}"
    )


_JSON_ADAPTER: TypeAdapter[JsonValue] = TypeAdapter(JsonValue)


def _get_ports(*, client: DevServerClient) -> PortsResponse:
    import httpx

    with httpx.Client(transport=client.transport, timeout=client.timeout) as http:
        resp = http.get(f"{client.base_url}/ports")
        resp.raise_for_status()
        return PortsResponse.model_validate(resp.json())


def _get_backend_base_url(*, client: DevServerClient) -> str:
    """Return backend base url like http://host:port using dev server config."""
    data = _get_ports(client=client)
    return f"http://{data.host}:{data.backend_port}"


def _get_backend_base_url_safe(
    *, manager: DevManager
) -> tuple[str | None, McpErrorResponse | None]:
    """Get backend base URL or return a typed error response."""
    if not manager.is_dev_server_running():
        return None, McpErrorResponse(error="Dev server is not running")

    client = DevServerClient(manager.socket_path)
    try:
        status = client.status()
        if not status.backend_running:
            return None, McpErrorResponse(error="Backend is not running")
        backend_url = _get_backend_base_url(client=client)
        return backend_url, None
    except Exception as e:
        return None, McpErrorResponse(
            error=f"Failed to determine backend URL from dev server: {str(e)}"
        )


def _fetch_backend_openapi_schema(
    *, backend_url: str, timeout_seconds: float = 10.0
) -> JsonObject:
    """Fetch backend OpenAPI schema from /openapi.json."""
    import httpx

    with httpx.Client(timeout=timeout_seconds) as client:
        resp = client.get(f"{backend_url}/openapi.json")
        resp.raise_for_status()
        parsed = _JSON_ADAPTER.validate_python(resp.json())
        if not isinstance(parsed, dict):
            raise ValueError("OpenAPI schema is not a JSON object")
        return cast(JsonObject, parsed)


@mcp.resource("apx://backend/openapi")
async def backend_openapi() -> McpOpenApiSchemaResponse | McpErrorResponse:
    """Return the backend FastAPI OpenAPI schema as a structured object."""
    manager = _get_manager()
    backend_url, err = await asyncio.to_thread(
        _get_backend_base_url_safe, manager=manager
    )
    if err is not None or backend_url is None:
        return err or McpErrorResponse(error="Failed to determine backend URL")

    try:
        schema = await asyncio.to_thread(
            _fetch_backend_openapi_schema, backend_url=backend_url
        )
        return McpOpenApiSchemaResponse(backend_url=backend_url, openapi_schema=schema)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to fetch OpenAPI schema: {str(e)}")


@mcp.resource("apx://backend/routes")
async def backend_routes() -> McpRoutesResponse | McpErrorResponse:
    """List available backend routes derived from the OpenAPI schema."""
    manager = _get_manager()
    backend_url, err = await asyncio.to_thread(
        _get_backend_base_url_safe, manager=manager
    )
    if err is not None or backend_url is None:
        return err or McpErrorResponse(error="Failed to determine backend URL")

    try:
        schema = await asyncio.to_thread(
            _fetch_backend_openapi_schema, backend_url=backend_url
        )
        paths_any = schema.get("paths", {})
        paths: dict[str, JsonValue]
        if isinstance(paths_any, dict):
            paths = cast(dict[str, JsonValue], paths_any)
        else:
            paths = {}

        routes: list[RouteInfo] = []
        for path, ops_any in paths.items():
            if not isinstance(ops_any, dict):
                continue
            ops = cast(dict[str, JsonValue], ops_any)
            methods: list[str] = []
            operation_ids: list[str] = []
            summaries: list[str] = []

            for method, op_any in ops.items():
                # OpenAPI uses lowercase methods in "paths"
                method_upper = str(method).upper()
                if method_upper not in {
                    "GET",
                    "POST",
                    "PUT",
                    "PATCH",
                    "DELETE",
                    "HEAD",
                    "OPTIONS",
                }:
                    continue
                methods.append(method_upper)
                op: dict[str, JsonValue]
                if isinstance(op_any, dict):
                    op = cast(dict[str, JsonValue], op_any)
                else:
                    op = {}

                operation_id = op.get("operationId")
                if isinstance(operation_id, str):
                    operation_ids.append(operation_id)

                summary = op.get("summary")
                if isinstance(summary, str):
                    summaries.append(summary)

            if methods:
                routes.append(
                    RouteInfo(
                        path=str(path),
                        methods=sorted(set(methods)),
                        operation_ids=operation_ids,
                        summaries=summaries,
                    )
                )

        routes.sort(key=lambda r: r.path)
        return McpRoutesResponse(backend_url=backend_url, routes=routes)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to list routes: {str(e)}")


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

        # Get frontend port/host from dev server
        client = DevServerClient(manager.socket_path)
        status_data = await asyncio.to_thread(client.status)
        ports_data = await asyncio.to_thread(_get_ports, client=client)
        host = ports_data.host

        return McpUrlResponse(url=f"http://{host}:{status_data.frontend_port}")
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


@mcp.tool()
async def call_route(
    method: Literal[
        "GET",
        "POST",
        "PUT",
        "PATCH",
        "DELETE",
        "HEAD",
        "OPTIONS",
    ],
    path: str,
    query: dict[str, str | int | float | bool] | None = None,
    headers: dict[str, str] | None = None,
    json_body: JsonValue | None = None,
    text_body: str | None = None,
    timeout_seconds: float = 30.0,
) -> McpRouteCallResponse | McpErrorResponse:
    """Call a backend route and return the HTTP response.

    Args:
        method: HTTP method (e.g., GET/POST)
        path: Route path (e.g., /api/items). If it doesn't start with '/', it will be added.
        query: Query parameters to include
        headers: Request headers to include
        json_body: JSON body (for POST/PUT/PATCH)
        text_body: Text body (alternative to json_body)
        timeout_seconds: Request timeout in seconds
    """
    manager = _get_manager()
    backend_url, err = await asyncio.to_thread(
        _get_backend_base_url_safe, manager=manager
    )
    if err is not None or backend_url is None:
        return err or McpErrorResponse(error="Failed to determine backend URL")

    if not path.startswith("/"):
        path = f"/{path}"

    def do_request() -> McpRouteCallResponse:
        import httpx

        url = f"{backend_url}{path}"
        with httpx.Client(timeout=timeout_seconds) as client:
            resp = client.request(
                method=method,
                url=url,
                params=query,
                headers=headers,
                json=json_body if json_body is not None else None,
                content=None if json_body is not None else text_body,
            )

            parsed_json: JsonValue | None = None
            try:
                parsed_json = _JSON_ADAPTER.validate_python(resp.json())
            except Exception:
                parsed_json = None

            return McpRouteCallResponse(
                request_url=str(resp.request.url),
                method=method,
                status_code=resp.status_code,
                headers={k: v for k, v in resp.headers.items()},
                text=resp.text,
                json_body=parsed_json,
            )

    try:
        return await asyncio.to_thread(do_request)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to call route: {str(e)}")


@mcp.tool()
async def dev_check(
    app_dir: str | None = None,
    max_output_chars: int = 20000,
) -> McpDevCheckResponse | McpErrorResponse:
    """Run the equivalent of `apx dev check` and return structured results.

    This checks:
    - TypeScript: `bun run tsc -b --incremental`
    - Python: `uv run basedpyright --level error`
    """
    cwd = Path(app_dir) if app_dir else Path.cwd()

    def run_one(name: str, cmd: list[str]) -> CheckCommandResult:
        start = time.time()
        result = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
        )
        duration_ms = int((time.time() - start) * 1000)
        return CheckCommandResult(
            name=name,
            command=cmd,
            cwd=str(cwd),
            returncode=result.returncode,
            stdout=_truncate(result.stdout or "", max_output_chars),
            stderr=_truncate(result.stderr or "", max_output_chars),
            duration_ms=duration_ms,
        )

    try:
        tsc = await asyncio.to_thread(
            run_one, "tsc", ["bun", "run", "tsc", "-b", "--incremental"]
        )
        pyright = await asyncio.to_thread(
            run_one, "basedpyright", ["uv", "run", "basedpyright", "--level", "error"]
        )
        success = (tsc.returncode == 0) and (pyright.returncode == 0)

        return McpDevCheckResponse(success=success, tsc=tsc, pyright=pyright)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to run dev check: {str(e)}")


def run_mcp_server() -> None:
    """Run the MCP server using stdio transport."""
    # FastMCP.run() automatically uses stdio when called without arguments
    mcp.run()
