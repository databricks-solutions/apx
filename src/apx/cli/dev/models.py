"""Data models for client-server communication in the dev module."""

from typing import Literal

from pydantic import BaseModel, Field


# === Log Models ===


class LogEntry(BaseModel):
    """Strongly typed log entry model for streaming logs."""

    timestamp: str
    level: str
    process_name: str
    content: str


# === Process Management Models ===


class DevConfig(BaseModel):
    """Dev server configuration."""

    token_id: str | None = None


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
