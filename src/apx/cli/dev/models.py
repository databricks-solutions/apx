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
    pid: int | None = None
    port: int | None = None


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
