"""Common MCP tools for apx dev server management.

MCP tools call CLI commands via subprocess to ensure identical behavior
between CLI and MCP interfaces.
"""

import asyncio
import os
import subprocess
import time
from pathlib import Path
from typing import Literal, cast

import httpx
import yaml
from dotenv import load_dotenv
from pydantic import BaseModel, JsonValue, TypeAdapter

from apx import __version__ as apx_version
from apx.cli.dev.client import DevServerClient
from apx.cli.dev.core import DevCore
from apx.mcp.server import mcp
from apx.models import (
    CheckCommandResult,
    JsonObject,
    McpActionResponse,
    McpDatabricksAppsLogsResponse,
    McpDevCheckResponse,
    McpDevLogsResponse,
    McpErrorResponse,
    McpMetadataResponse,
    McpOpenApiSchemaResponse,
    McpRouteCallResponse,
    McpRoutesResponse,
    OpenApiStatusResponse,
    PortsResponse,
    ProjectMetadata,
    RouteInfo,
)


class McpSimpleStatusResponse(BaseModel):
    """Simplified MCP status response focused on dev server URL."""

    dev_server_running: bool = False
    dev_server_url: str | None = None
    api_prefix: str | None = None
    frontend_running: bool = False
    backend_running: bool = False
    openapi_running: bool = False


def _get_core() -> DevCore:
    """Get DevCore instance for the current project directory."""
    return DevCore(Path.cwd())


def _get_dev_server_client() -> DevServerClient | None:
    """Get DevServerClient if dev server is running, None otherwise."""
    core = _get_core()
    config = core.get_or_create_config()
    port = config.dev.dev_server_port

    if port is None or not core.is_running():
        return None

    return DevServerClient(port=port)


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


def _resolve_app_name_from_databricks_yml(*, project_dir: Path) -> str:
    """Resolve app name from databricks.yml in the project root.

    Looks for exactly one app under resources.apps.*.name.
    """
    yml_path = project_dir / "databricks.yml"
    if not yml_path.exists():
        raise ValueError(
            (
                f"Could not auto-detect app name because databricks.yml was not found at {yml_path}. "
                "Please pass app_name explicitly."
            )
        )

    try:
        data_any = cast(object, yaml.safe_load(yml_path.read_text(encoding="utf-8")))
    except Exception as e:
        raise ValueError(f"Failed to parse databricks.yml: {e}") from e

    if not isinstance(data_any, dict):
        raise ValueError("databricks.yml root must be a mapping/object")

    data = cast(dict[str, object], data_any)

    resources_any: object = data.get("resources", {})
    if not isinstance(resources_any, dict):
        raise ValueError("databricks.yml 'resources' must be a mapping/object")

    resources = cast(dict[str, object], resources_any)

    apps_any: object = resources.get("apps", {})
    if not isinstance(apps_any, dict):
        raise ValueError("databricks.yml 'resources.apps' must be a mapping/object")

    apps = cast(dict[str, object], apps_any)

    app_names: list[str] = []
    for app_def_any in apps.values():
        if not isinstance(app_def_any, dict):
            continue
        app_def = cast(dict[str, object], app_def_any)
        name_any: object = app_def.get("name")
        if isinstance(name_any, str) and name_any.strip():
            app_names.append(name_any.strip())

    app_names = sorted(set(app_names))

    if len(app_names) == 1:
        return app_names[0]
    if len(app_names) == 0:
        raise ValueError(
            (
                "Could not auto-detect app name because no apps were found in databricks.yml under "
                "resources.apps.*.name. Please pass app_name explicitly."
            )
        )
    raise ValueError(
        (
            "Could not auto-detect app name because multiple apps were found in databricks.yml "
            f"({', '.join(app_names)}). Please pass app_name explicitly."
        )
    )


async def _run_cli(
    *, cmd: list[str], cwd: Path, timeout_seconds: float | None
) -> tuple[int, str, str, int]:
    """Run a CLI command asynchronously and capture stdout/stderr."""
    start = time.time()
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        cwd=str(cwd),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env=os.environ,
    )

    try:
        if timeout_seconds is not None and timeout_seconds > 0:
            stdout_b, stderr_b = await asyncio.wait_for(
                proc.communicate(), timeout=timeout_seconds
            )
        else:
            stdout_b, stderr_b = await proc.communicate()
    except asyncio.TimeoutError:
        try:
            proc.kill()
        except Exception:
            pass
        raise

    duration_ms = int((time.time() - start) * 1000)
    stdout = (stdout_b or b"").decode("utf-8", errors="replace")
    stderr = (stderr_b or b"").decode("utf-8", errors="replace")
    return proc.returncode or 0, stdout, stderr, duration_ms


def _get_ports(*, client: DevServerClient) -> PortsResponse:
    with httpx.Client(timeout=client.timeout) as http:
        resp = http.get(f"{client.base_url}/__apx__/ports")
        resp.raise_for_status()
        return PortsResponse.model_validate(resp.json())


def _get_backend_base_url(*, client: DevServerClient) -> str:
    """Return backend base url like http://host:port using dev server config."""
    data = _get_ports(client=client)
    return f"http://{data.host}:{data.backend_port}"


def _get_backend_base_url_safe(
    *, core: DevCore
) -> tuple[str | None, McpErrorResponse | None]:
    """Get backend base URL or return a typed error response."""
    config = core.get_or_create_config()
    port = config.dev.dev_server_port

    if port is None or not core.is_running():
        return None, McpErrorResponse(error="Dev server is not running")

    client = DevServerClient(port=port)
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


def _get_dev_server_url_safe(
    *, core: DevCore
) -> tuple[str | None, str | None, McpErrorResponse | None]:
    """Get dev server URL and api_prefix, or return a typed error response.

    Returns:
        Tuple of (dev_server_url, api_prefix, error).
    """
    config = core.get_or_create_config()
    port = config.dev.dev_server_port

    if port is None or not core.is_running():
        return None, None, McpErrorResponse(error="Dev server is not running")

    client = DevServerClient(port=port)
    try:
        ports_data = _get_ports(client=client)
        dev_server_url = f"http://{ports_data.host}:{port}"
        return dev_server_url, ports_data.api_prefix, None
    except Exception as e:
        return (
            None,
            None,
            McpErrorResponse(error=f"Failed to get dev server info: {str(e)}"),
        )


def _fetch_backend_openapi_schema(
    *, backend_url: str, timeout_seconds: float = 10.0
) -> JsonObject:
    """Fetch backend OpenAPI schema from /openapi.json."""
    with httpx.Client(timeout=timeout_seconds) as client:
        resp = client.get(f"{backend_url}/openapi.json")
        resp.raise_for_status()
        parsed = _JSON_ADAPTER.validate_python(resp.json())
        if not isinstance(parsed, dict):
            raise ValueError("OpenAPI schema is not a JSON object")
        return cast(JsonObject, parsed)


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
- **status**: Get dev server status and URL
- **refresh_openapi**: Trigger OpenAPI schema and api.ts regeneration
- **list_routes**: List available backend API routes
- **call_route**: Call a backend route through the dev server proxy
- **get_metadata**: Get project metadata from pyproject.toml

Databricks SDK documentation tools:
- **search_databricks_sdk**: Search SDK methods by natural language query
- **get_method_spec**: Get detailed specification for a specific SDK method

Resources:
- **apx://info**: Information about apx toolkit
- **apx://backend/openapi**: Backend OpenAPI schema
- **apx://openapi/status**: OpenAPI regeneration timestamps

Use these tools to interact with your apx project during development."""


@mcp.resource("apx://backend/openapi")
async def backend_openapi() -> McpOpenApiSchemaResponse | McpErrorResponse:
    """Return the backend FastAPI OpenAPI schema as a structured object."""
    core = _get_core()
    backend_url, err = await asyncio.to_thread(_get_backend_base_url_safe, core=core)
    if err is not None or backend_url is None:
        return err or McpErrorResponse(error="Failed to determine backend URL")

    try:
        schema = await asyncio.to_thread(
            _fetch_backend_openapi_schema, backend_url=backend_url
        )
        return McpOpenApiSchemaResponse(backend_url=backend_url, openapi_schema=schema)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to fetch OpenAPI schema: {str(e)}")


@mcp.resource("apx://openapi/status")
async def openapi_status() -> OpenApiStatusResponse | McpErrorResponse:
    """Get OpenAPI regeneration status and timestamps.

    Returns information about the OpenAPI schema and api.ts client files,
    including their paths and last regeneration timestamps.
    """
    core = _get_core()
    dev_server_url, _, err = await asyncio.to_thread(
        _get_dev_server_url_safe, core=core
    )
    if err is not None or dev_server_url is None:
        return err or McpErrorResponse(error="Dev server is not running")

    def fetch_status() -> OpenApiStatusResponse:
        with httpx.Client(timeout=10.0) as client:
            resp = client.get(f"{dev_server_url}/__apx__/openapi-status")
            resp.raise_for_status()
            return OpenApiStatusResponse.model_validate(resp.json())

    try:
        return await asyncio.to_thread(fetch_status)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to get OpenAPI status: {str(e)}")


@mcp.tool()
async def start(
    host: str | None = None,
    api_prefix: str | None = None,
    obo: bool | None = None,
    openapi: bool | None = None,
    max_retries: int | None = None,
) -> McpActionResponse:
    """Start development servers (frontend, backend, and optionally OpenAPI watcher).

    The dev server will find available ports automatically:
    - Dev server (reverse proxy): 9000-9999
    - Frontend: 5000-5999
    - Backend: 8000-8999

    Args:
        host: Host for dev, frontend, and backend servers (default: localhost)
        api_prefix: URL prefix for API routes (default: /api)
        obo: Whether to add On-Behalf-Of header to the backend server (default: True)
            This enables OBO token generation for Databricks API calls
        openapi: Whether to start OpenAPI watcher process (default: True)
        max_retries: Maximum number of retry attempts for processes (default: 10)

    Returns:
        McpActionResponse with status and message indicating success or failure
    """
    # Build CLI command with arguments
    cmd: list[str] = ["uv", "run", "apx", "dev", "start"]

    if host is not None:
        cmd.extend(["--host", host])
    if api_prefix is not None:
        cmd.extend(["--api-prefix", api_prefix])
    if obo is not None:
        cmd.append(f"--{'obo' if obo else 'no-obo'}")
    if openapi is not None:
        cmd.append(f"--{'openapi' if openapi else 'no-openapi'}")
    if max_retries is not None:
        cmd.extend(["--max-retries", str(max_retries)])

    try:
        # Run CLI command via subprocess
        result = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=Path.cwd(),
        )
        stdout, stderr = await result.communicate()

        if result.returncode == 0:
            return McpActionResponse(
                status="success", message="Development servers started successfully"
            )
        else:
            error_msg = stderr.decode("utf-8", errors="replace").strip()
            if not error_msg:
                error_msg = stdout.decode("utf-8", errors="replace").strip()
            return McpActionResponse(
                status="error",
                message=f"Failed to start servers: {error_msg or 'Unknown error'}",
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
    # Run CLI command via subprocess
    cmd = ["uv", "run", "apx", "dev", "restart"]

    try:
        result = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=Path.cwd(),
        )
        stdout, stderr = await result.communicate()

        if result.returncode == 0:
            return McpActionResponse(
                status="success", message="Development servers restarted successfully"
            )
        else:
            error_msg = stderr.decode("utf-8", errors="replace").strip()
            if not error_msg:
                error_msg = stdout.decode("utf-8", errors="replace").strip()
            return McpActionResponse(
                status="error",
                message=f"Failed to restart servers: {error_msg or 'Unknown error'}",
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
    # Run CLI command via subprocess
    cmd = ["uv", "run", "apx", "dev", "stop"]

    try:
        result = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=Path.cwd(),
        )
        stdout, stderr = await result.communicate()

        if result.returncode == 0:
            return McpActionResponse(
                status="success", message="Development servers stopped successfully"
            )
        else:
            error_msg = stderr.decode("utf-8", errors="replace").strip()
            if not error_msg:
                error_msg = stdout.decode("utf-8", errors="replace").strip()
            return McpActionResponse(
                status="error",
                message=f"Failed to stop servers: {error_msg or 'Unknown error'}",
            )
    except Exception as e:
        return McpActionResponse(
            status="error", message=f"Failed to stop servers: {str(e)}"
        )


@mcp.tool()
async def refresh_openapi() -> McpActionResponse:
    """Trigger OpenAPI schema and api.ts client regeneration.

    Forces regeneration of:
    - OpenAPI schema (.apx/openapi.json)
    - TypeScript API client (src/<app>/ui/lib/api.ts)

    This is useful when you want to manually refresh the API client after
    making changes to the backend API without waiting for the file watcher.

    Returns:
        McpActionResponse with status and message indicating success or failure
    """
    core = _get_core()
    dev_server_url, _, err = await asyncio.to_thread(
        _get_dev_server_url_safe, core=core
    )
    if err is not None or dev_server_url is None:
        return McpActionResponse(
            status="error",
            message=err.error if err else "Dev server is not running",
        )

    def do_refresh() -> McpActionResponse:
        with httpx.Client(timeout=60.0) as client:
            resp = client.post(f"{dev_server_url}/__apx__/actions/refresh-openapi")
            if resp.status_code != 200:
                return McpActionResponse(
                    status="error",
                    message=f"Failed to refresh OpenAPI: HTTP {resp.status_code}",
                )
            data = resp.json()
            return McpActionResponse(
                status=data.get("status", "error"),
                message=data.get("message", "Unknown response"),
            )

    try:
        return await asyncio.to_thread(do_refresh)
    except Exception as e:
        return McpActionResponse(
            status="error", message=f"Failed to refresh OpenAPI: {str(e)}"
        )


@mcp.tool()
async def status() -> McpSimpleStatusResponse:
    """Get the status of development servers.

    Returns simplified status focused on the dev server URL (the single entry point).

    Returns:
        McpSimpleStatusResponse with status information including:
        - dev_server_running: Whether the dev server is running
        - dev_server_url: The URL of the dev server (e.g., http://localhost:9000)
        - api_prefix: The API prefix used for backend routes (e.g., /api)
        - frontend_running: Whether the frontend server is running
        - backend_running: Whether the backend server is running
        - openapi_running: Whether the OpenAPI watcher is running
    """
    core = _get_core()

    # Initialize with default values
    result = McpSimpleStatusResponse(
        dev_server_running=False,
        dev_server_url=None,
        api_prefix=None,
        frontend_running=False,
        backend_running=False,
        openapi_running=False,
    )

    is_running = await asyncio.to_thread(core.is_running)
    if not is_running:
        return result

    config = core.get_or_create_config()
    port = config.dev.dev_server_port

    result.dev_server_running = True

    # Try to get status from dev server
    if port is not None:
        client = DevServerClient(port=port)
        try:
            ports_data = await asyncio.to_thread(_get_ports, client=client)
            result.dev_server_url = f"http://{ports_data.host}:{port}"
            result.api_prefix = ports_data.api_prefix

            status_data = await asyncio.to_thread(client.status)
            result.frontend_running = status_data.frontend_running
            result.backend_running = status_data.backend_running
            result.openapi_running = status_data.openapi_running
        except Exception:
            # Dev server is running but not responding - likely still starting
            result.dev_server_url = f"http://localhost:{port}"

    return result


@mcp.tool()
async def dev_logs(
    duration_seconds: int | None = None,
    channel: Literal["app", "ui", "apx", "all"] = "all",
    component: str | None = None,
    include_system: bool = False,
    limit: int = 500,
) -> McpDevLogsResponse | McpErrorResponse:
    """Fetch a bounded snapshot of dev logs.

    Notes:
    - By default, system logs ([apx]) are excluded.
    - Set include_system=True to include [apx] logs in addition to app/ui logs.
    - If channel='apx', only [apx] logs are returned regardless of include_system.
    """
    client = _get_dev_server_client()
    if client is None:
        return McpErrorResponse(error="Dev server is not running")

    def fetch() -> McpDevLogsResponse:
        logs = client.get_logs_snapshot(
            duration=duration_seconds,
            channel=channel,
            component=component,
            include_system=include_system,
            limit=limit,
        )
        return McpDevLogsResponse(logs=logs)

    try:
        return await asyncio.to_thread(fetch)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to fetch dev logs: {str(e)}")


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
        metadata = await asyncio.to_thread(ProjectMetadata.read)
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
    """Call a backend route through the dev server proxy.

    The request goes through the dev server which proxies to the backend.
    API routes should use the configured api_prefix (default: /api).

    Args:
        method: HTTP method (e.g., GET/POST)
        path: Route path (e.g., /api/items or /api/health).
              If it doesn't start with '/', it will be added.
              Use the api_prefix (typically /api) for backend routes.
        query: Query parameters to include
        headers: Request headers to include
        json_body: JSON body (for POST/PUT/PATCH)
        text_body: Text body (alternative to json_body)
        timeout_seconds: Request timeout in seconds
    """
    core = _get_core()
    dev_server_url, _api_prefix, err = await asyncio.to_thread(
        _get_dev_server_url_safe, core=core
    )
    if err is not None or dev_server_url is None:
        return err or McpErrorResponse(error="Dev server is not running")

    if not path.startswith("/"):
        path = f"/{path}"

    def do_request() -> McpRouteCallResponse:
        url = f"{dev_server_url}{path}"
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
async def list_routes() -> McpRoutesResponse | McpErrorResponse:
    """List available backend API routes.

    Returns a list of routes derived from the backend's OpenAPI schema,
    including paths, HTTP methods, operation IDs, and summaries.

    Returns:
        McpRoutesResponse with the list of available routes
    """
    core = _get_core()
    dev_server_url, _api_prefix, err = await asyncio.to_thread(
        _get_dev_server_url_safe, core=core
    )
    if err is not None or dev_server_url is None:
        return err or McpErrorResponse(error="Dev server is not running")

    # Need backend URL for fetching OpenAPI schema
    backend_url, backend_err = await asyncio.to_thread(
        _get_backend_base_url_safe, core=core
    )
    if backend_err is not None or backend_url is None:
        return backend_err or McpErrorResponse(error="Backend is not running")

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

            for method_key, op_any in ops.items():
                # OpenAPI uses lowercase methods in "paths"
                method_upper = str(method_key).upper()
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
        # Use dev_server_url as the base URL since that's where clients should call
        return McpRoutesResponse(backend_url=dev_server_url, routes=routes)
    except Exception as e:
        return McpErrorResponse(error=f"Failed to list routes: {str(e)}")


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


@mcp.tool()
async def databricks_apps_logs(
    app_name: str | None = None,
    tail_lines: int = 200,
    search: str | None = None,
    source: list[Literal["APP", "SYSTEM"]] | None = None,
    profile: str | None = None,
    target: str | None = None,
    output: Literal["text", "json"] = "text",
    timeout_seconds: float = 60.0,
    max_output_chars: int = 20000,
) -> McpDatabricksAppsLogsResponse | McpErrorResponse:
    """Fetch Databricks Apps logs in production via the Databricks CLI.

    Uses: `databricks apps logs NAME [flags]`

    Args:
        app_name: Databricks App name. If omitted, resolves from `databricks.yml` in CWD
                  by requiring exactly one app under `resources.apps.*.name`.
        tail_lines: Number of recent log lines to show (default: 200).
        search: Optional server-side search term.
        source: Optional list of sources to include (APP and/or SYSTEM).
        profile: Optional Databricks CLI profile (`-p`).
        target: Optional bundle target (`-t`) if applicable.
        output: Databricks CLI output format (`-o`), text or json (default: text).
        timeout_seconds: Max time to wait for the CLI command to finish.
        max_output_chars: Truncate stdout/stderr to this many characters.
    """
    cwd = Path.cwd()
    resolved_from_yml = False

    try:
        # Load env vars from .env if present (common for DATABRICKS_CONFIG_PROFILE, etc.)
        dotenv_path = cwd / ".env"
        if dotenv_path.exists():
            load_dotenv(dotenv_path)

        if app_name is None or not app_name.strip():
            try:
                app_name = _resolve_app_name_from_databricks_yml(project_dir=cwd)
                resolved_from_yml = True
            except ValueError as e:
                # Explicit error handling for app-name auto-detection failures
                return McpErrorResponse(error=str(e))
        else:
            app_name = app_name.strip()

        cmd: list[str] = ["databricks", "apps", "logs", app_name]
        cmd += ["--tail-lines", str(tail_lines)]
        if search is not None and search.strip():
            cmd += ["--search", search.strip()]
        if source:
            for s in source:
                cmd += ["--source", s]
        if profile is not None and profile.strip():
            cmd += ["-p", profile.strip()]
        if target is not None and target.strip():
            cmd += ["-t", target.strip()]
        cmd += ["-o", output]

        try:
            returncode, stdout, stderr, duration_ms = await _run_cli(
                cmd=cmd, cwd=cwd, timeout_seconds=timeout_seconds
            )
        except FileNotFoundError:
            return McpErrorResponse(
                error=(
                    "Databricks CLI executable not found (`databricks`). "
                    "Please install Databricks CLI v0.280.0 or higher and ensure it's on PATH."
                )
            )
        except asyncio.TimeoutError:
            return McpErrorResponse(
                error=f"Timed out after {timeout_seconds}s running: {' '.join(cmd)}"
            )

        stdout_t = _truncate(stdout, max_output_chars)
        stderr_t = _truncate(stderr, max_output_chars)

        if returncode != 0:
            combined = f"{stderr}\n{stdout}".lower()
            # Explicit error handling: `databricks apps logs` subcommand not available
            if (
                'unknown command "logs"' in combined
                or "unknown command logs" in combined
                or "unknown subcommand" in combined
                or "no such command" in combined
            ):
                return McpErrorResponse(
                    error=(
                        "Databricks CLI does not support `databricks apps logs` in this version. "
                        "Please upgrade Databricks CLI to v0.280.0 or higher.\n\n"
                        f"Command: {' '.join(cmd)}\n"
                        f"Exit code: {returncode}\n"
                        f"stderr:\n{stderr_t}\n"
                        f"stdout:\n{stdout_t}"
                    )
                )

            # Forward any other CLI error
            return McpErrorResponse(
                error=(
                    f"`databricks apps logs` failed.\n\n"
                    f"Command: {' '.join(cmd)}\n"
                    f"Exit code: {returncode}\n"
                    f"stderr:\n{stderr_t}\n"
                    f"stdout:\n{stdout_t}"
                )
            )

        return McpDatabricksAppsLogsResponse(
            app_name=app_name,
            resolved_from_databricks_yml=resolved_from_yml,
            command=cmd,
            cwd=str(cwd),
            returncode=returncode,
            stdout=stdout_t,
            stderr=stderr_t,
            duration_ms=duration_ms,
        )
    except Exception as e:
        return McpErrorResponse(error=f"Failed to fetch Databricks app logs: {e}")
