"""Data models for client-server communication in the dev module."""

import time
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


class ProcessInfo(BaseModel):
    """Information about a running process."""

    pid: int
    port: int | None = None
    created_at: str = Field(default_factory=lambda: time.strftime("%Y-%m-%d %H:%M:%S"))


class ProjectConfig(BaseModel):
    """Configuration stored in .apx/project.json."""

    token_id: str | None = None
    dev_server_pid: int | None = None
    dev_server_port: int | None = None
    processes: dict[str, ProcessInfo] = Field(default_factory=dict)


# === API Request/Response Models ===


class ActionRequest(BaseModel):
    """Request model for action endpoints (start/restart)."""

    frontend_port: int = 5173
    backend_port: int = 8000
    backend_host: str = "0.0.0.0"
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
