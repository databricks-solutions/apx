"""Data models for client-server communication in the dev module."""

from __future__ import annotations

from typing import Literal, TypeAlias

from pydantic import BaseModel, Field, JsonValue


# === Log Models ===


class LogEntry(BaseModel):
    """Strongly typed log entry model for streaming logs."""

    timestamp: str
    level: str
    process_name: str
    content: str


# === Process Management Models ===


class DevProcessInfo(BaseModel):
    """Tracked process metadata for safe shutdown/restart.

    create_time protects against PID reuse. pgid enables POSIX process-group shutdown
    even if the original PID has already exited (common with bun -> node handoff).
    """

    pid: int | None = None
    create_time: float | None = None
    pgid: int | None = None


class DevConfig(BaseModel):
    """Dev server configuration."""

    token_id: str | None = None
    host: str | None = None
    frontend_port: int | None = None
    backend_port: int | None = None
    dev_server_process: DevProcessInfo | None = None
    frontend_process: DevProcessInfo | None = None


class ProjectConfig(BaseModel):
    """Configuration stored in .apx/project.json."""

    dev: DevConfig = Field(default_factory=DevConfig)


# === API Request/Response Models ===


class ActionRequest(BaseModel):
    """Request model for action endpoints (start/restart)."""

    frontend_port: int = 5173
    backend_port: int = 8000
    host: str = "localhost"
    obo: bool = True
    openapi: bool = True
    max_retries: int = 10


class ActionResponse(BaseModel):
    """Response model for action endpoints."""

    status: Literal["success", "error"]
    message: str


class StatusResponse(BaseModel):
    """Response model for status endpoint."""

    frontend_running: bool
    backend_running: bool
    openapi_running: bool
    frontend_port: int
    backend_port: int


class PortsResponse(BaseModel):
    """Response model for ports endpoint."""

    frontend_port: int
    backend_port: int
    host: str


# === MCP Response Models ===


class McpActionResponse(BaseModel):
    """MCP response model for action endpoints (start/restart/stop).

    Reuses ActionResponse structure but named for MCP context.
    """

    status: Literal["success", "error"]
    message: str


class McpStatusResponse(BaseModel):
    """MCP response model for status endpoint.

    Extends StatusResponse with additional dev server information.
    """

    dev_server_running: bool
    dev_server_port: int | None = None
    dev_server_pid: int | None = None
    frontend_running: bool
    frontend_port: int | None = None
    backend_running: bool
    backend_port: int | None = None
    openapi_running: bool


class McpMetadataResponse(BaseModel):
    """MCP response model for project metadata."""

    app_name: str
    app_module: str
    app_slug: str
    apx_version: str


class McpErrorResponse(BaseModel):
    """MCP response model for errors."""

    error: str


class McpUrlResponse(BaseModel):
    """MCP response model for the frontend URL."""

    url: str


# === MCP Backend Introspection / Invocation Models ===


class McpOpenApiSchemaResponse(BaseModel):
    """MCP response model for fetching backend OpenAPI schema."""

    backend_url: str

    # NOTE: Don't name this field "schema" because it collides with BaseModel.schema().
    openapi_schema: JsonObject


class RouteInfo(BaseModel):
    """A single API route from an OpenAPI schema."""

    path: str
    methods: list[str]
    operation_ids: list[str] = Field(default_factory=list)
    summaries: list[str] = Field(default_factory=list)


class McpRoutesResponse(BaseModel):
    """MCP response model for listing backend routes."""

    backend_url: str
    routes: list[RouteInfo]


HttpMethod = Literal[
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "HEAD",
    "OPTIONS",
]


class McpRouteCallResponse(BaseModel):
    """MCP response model for calling a backend route."""

    request_url: str
    method: HttpMethod
    status_code: int
    headers: dict[str, str]
    text: str | None = None
    # NOTE: Don't name this field "json" because it collides with BaseModel.json().
    json_body: JsonValue | None = None


class CheckCommandResult(BaseModel):
    """Result of running a single command as part of `apx dev check`."""

    name: str
    command: list[str]
    cwd: str
    returncode: int
    stdout: str
    stderr: str
    duration_ms: int


class McpDevCheckResponse(BaseModel):
    """MCP response model for running `apx dev check` (structured)."""

    success: bool
    tsc: CheckCommandResult
    pyright: CheckCommandResult


JsonObject: TypeAlias = dict[str, JsonValue]
