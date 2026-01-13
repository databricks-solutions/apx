"""Centralized Pydantic models, enums, and type aliases for apx."""

from __future__ import annotations

import asyncio
import tomllib
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, Literal, Protocol, TypeAlias

from pydantic import BaseModel, ConfigDict, Field, JsonValue

if TYPE_CHECKING:
    from fastapi import FastAPI

from apx.constants import (
    BACKEND_PORT_START,
    DEFAULT_API_PREFIX,
    DEFAULT_HOST,
    DEFAULT_MAX_RETRIES,
    DEV_SERVER_PORT_START,
    FRONTEND_PORT_START,
)


# === Type Aliases ===

JsonObject: TypeAlias = dict[str, JsonValue]

HttpMethod = Literal[
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "HEAD",
    "OPTIONS",
]

ActionStatus = Literal["success", "error"]


# === Enums ===


class Template(str, Enum):
    """Template options for project initialization."""

    essential = "essential"
    stateful = "stateful"

    @classmethod
    def from_string(cls, value: str) -> Template:
        try:
            return cls(value.lower())
        except ValueError:
            raise ValueError(f"Invalid template: {value}")


class Assistant(str, Enum):
    """AI assistant options for project initialization."""

    cursor = "cursor"
    vscode = "vscode"
    codex = "codex"
    claude = "claude"

    @classmethod
    def from_string(cls, value: str) -> Assistant:
        try:
            return cls(value.lower())
        except ValueError:
            raise ValueError(f"Invalid assistant: {value}")


class Layout(str, Enum):
    """UI layout options for project initialization."""

    basic = "basic"
    sidebar = "sidebar"

    @classmethod
    def from_string(cls, value: str) -> Layout:
        try:
            return cls(value.lower())
        except ValueError:
            raise ValueError(f"Invalid layout: {value}")


class StreamEvent(str, Enum):
    """Marker for special SSE events."""

    BUFFERED_DONE = "buffered_done"
    SERVER_SHUTDOWN = "server_shutdown"


# === Base Models (Building Blocks) ===


class TrackedProcess(BaseModel):
    """A process we started and are allowed to manage.

    create_time protects against PID reuse. pgid enables POSIX process-group shutdown
    even if the original PID has already exited (common with bun -> node handoff).
    """

    pid: int | None = None
    create_time: float | None = None
    pgid: int | None = None

    model_config: ClassVar[ConfigDict] = ConfigDict(frozen=True)


# DevProcessInfo is now an alias for TrackedProcess (they were identical)
DevProcessInfo = TrackedProcess


class FrontendProcessState(Protocol):
    """Minimal state surface needed by `run_frontend_with_logging`.

    This Protocol exists to avoid circular imports between server.py and manager.py.
    """

    frontend_process: asyncio.subprocess.Process | None
    frontend_tracked: TrackedProcess | None


class PortsConfig(BaseModel):
    """Port configuration for development servers."""

    dev_server_port: int = DEV_SERVER_PORT_START
    frontend_port: int = FRONTEND_PORT_START
    backend_port: int = BACKEND_PORT_START


class DevServerConfig(BaseModel):
    """Complete configuration for the development server.

    This is the single source of truth for all dev server configuration.
    All default values are defined here and should not be repeated elsewhere.
    """

    ports: PortsConfig = Field(default_factory=PortsConfig)
    host: str = DEFAULT_HOST
    api_prefix: str = DEFAULT_API_PREFIX
    obo: bool = True
    openapi: bool = True
    max_retries: int = DEFAULT_MAX_RETRIES
    watch: bool = False

    # Convenience properties for flat access (backwards compatibility)
    @property
    def dev_server_port(self) -> int:
        """Get dev server port from ports config."""
        return self.ports.dev_server_port

    @property
    def frontend_port(self) -> int:
        """Get frontend port from ports config."""
        return self.ports.frontend_port

    @property
    def backend_port(self) -> int:
        """Get backend port from ports config."""
        return self.ports.backend_port


class ProcessRunningStatus(BaseModel):
    """Running status flags for development server processes."""

    frontend_running: bool = False
    backend_running: bool = False
    openapi_running: bool = False
    frontend_exit_code: int | None = None
    backend_exit_code: int | None = None


class CommandResult(BaseModel):
    """Result of running a shell command."""

    command: list[str]
    cwd: str
    returncode: int
    stdout: str
    stderr: str
    duration_ms: int


# === Project Configuration Models ===


class ProjectMetadata(BaseModel):
    """Metadata about the project from pyproject.toml."""

    app_name: str = Field(description="The user-facing app name.", alias="app-name")
    app_module: str = Field(
        description="The internal app module name.", alias="app-module"
    )
    app_slug: str = Field(description="The internal app slug.", alias="app-slug")
    api_prefix: str = Field(
        default=DEFAULT_API_PREFIX,
        description="URL prefix for API routes.",
        alias="api-prefix",
    )

    @classmethod
    def read(cls, src_dir: Path | None = None) -> ProjectMetadata:
        """Read project metadata from pyproject.toml.

        Args:
            src_dir: Directory containing pyproject.toml. If None, uses Path.cwd().

        Returns:
            ProjectMetadata instance

        Raises:
            FileNotFoundError: If pyproject.toml doesn't exist
            KeyError: If required metadata is missing
        """
        if src_dir is None:
            src_dir = Path.cwd()

        pyproject_path = src_dir / "pyproject.toml"
        if not pyproject_path.exists():
            raise FileNotFoundError(f"pyproject.toml not found at {pyproject_path}")

        with open(pyproject_path, "rb") as f:
            data = tomllib.load(f)

        return cls.model_validate(data["tool"]["apx"]["metadata"])

    def generate_metadata_file(self, app_path: Path) -> None:
        """Generate the _metadata.py file for the project.

        Args:
            app_path: Path to the application directory
        """
        # Read metadata-path from pyproject.toml (not part of ProjectMetadata model)
        pyproject_path = app_path / "pyproject.toml"
        pyproject_data = tomllib.loads(pyproject_path.read_text())
        metadata_path_str: str = pyproject_data["tool"]["apx"]["metadata"][
            "metadata-path"
        ]
        metadata_path = app_path / metadata_path_str

        metadata_path.write_text(
            "\n".join(
                [
                    f'app_name = "{self.app_name}"',
                    f'app_module = "{self.app_module}"',
                    f'app_slug = "{self.app_slug}"',
                    f'api_prefix = "{self.api_prefix}"',
                ]
            )
        )

    def get_app_instance(self, *, reload: bool = False) -> "FastAPI":
        """Get the FastAPI app instance from the app_module.

        Parses app_module (e.g., "jview.backend.app:app") and imports the app.

        Args:
            reload: If True, reload the module before getting the app instance.

        Returns:
            The FastAPI application instance.

        Raises:
            ValueError: If app_module format is invalid.
            ImportError: If module cannot be imported.
            AttributeError: If module doesn't have the specified attribute.
        """
        import importlib

        from fastapi import FastAPI

        if ":" not in self.app_module:
            raise ValueError(
                f"Invalid app_module format '{self.app_module}'. "
                "Expected format: 'module.path:app_instance'"
            )

        module_path, attr_name = self.app_module.split(":", 1)

        module = importlib.import_module(module_path)
        if reload:
            module = importlib.reload(module)

        app_instance = getattr(module, attr_name)

        if not isinstance(app_instance, FastAPI):
            raise TypeError(f"'{attr_name}' in {module_path} is not a FastAPI instance")

        return app_instance


class DevConfig(BaseModel):
    """Dev server configuration stored in .apx/project.json.

    Simplified to only track the dev server process and token info.
    The dev server itself manages frontend/backend processes internally.
    """

    token_id: str | None = None
    dev_server_pid: int | None = None
    dev_server_port: int | None = None
    api_prefix: str = DEFAULT_API_PREFIX


class ProjectConfig(BaseModel):
    """Configuration stored in .apx/project.json."""

    dev: DevConfig = Field(default_factory=DevConfig)


# === Log Models ===


class LogChannel(str, Enum):
    """Logical log channel for dev logging."""

    APX = "apx"
    APP = "app"
    UI = "ui"


class LogEntry(BaseModel):
    """Strongly typed log entry model for streaming logs."""

    timestamp: str
    level: str
    channel: LogChannel
    component: str
    content: str


class BrowserLogPayload(BaseModel):
    """Payload for browser logs sent from the frontend dev tools."""

    level: Literal["error", "warn"]
    source: Literal["console", "window", "promise"]
    message: str
    stack: str | None = None
    timestamp: int  # Unix timestamp in milliseconds


# === API Request/Response Models ===


class ActionRequest(BaseModel):
    """Request model for action endpoints (start/restart).

    This model wraps DevServerConfig for API compatibility while providing
    flat field access for JSON serialization.
    """

    dev_server_port: int = DEV_SERVER_PORT_START
    frontend_port: int = FRONTEND_PORT_START
    backend_port: int = BACKEND_PORT_START
    host: str = DEFAULT_HOST
    api_prefix: str = DEFAULT_API_PREFIX
    obo: bool = True
    openapi: bool = True
    max_retries: int = DEFAULT_MAX_RETRIES

    @property
    def ports(self) -> PortsConfig:
        """Extract ports configuration from request."""
        return PortsConfig(
            dev_server_port=self.dev_server_port,
            frontend_port=self.frontend_port,
            backend_port=self.backend_port,
        )

    def to_config(self) -> DevServerConfig:
        """Convert to DevServerConfig for internal use."""
        return DevServerConfig(
            ports=self.ports,
            host=self.host,
            api_prefix=self.api_prefix,
            obo=self.obo,
            openapi=self.openapi,
            max_retries=self.max_retries,
        )

    @classmethod
    def from_config(cls, config: DevServerConfig) -> ActionRequest:
        """Create from DevServerConfig."""
        return cls(
            dev_server_port=config.dev_server_port,
            frontend_port=config.frontend_port,
            backend_port=config.backend_port,
            host=config.host,
            api_prefix=config.api_prefix,
            obo=config.obo,
            openapi=config.openapi,
            max_retries=config.max_retries,
        )


class ActionResponse(BaseModel):
    """Response model for action endpoints."""

    status: ActionStatus
    message: str

    @classmethod
    def success(cls, message: str) -> ActionResponse:
        """Create a success response."""
        return cls(status="success", message=message)

    @classmethod
    def error(cls, message: str) -> ActionResponse:
        """Create an error response."""
        return cls(status="error", message=message)


# McpActionResponse is identical to ActionResponse - use alias
McpActionResponse = ActionResponse


class StatusResponse(ProcessRunningStatus):
    """Response model for status endpoint.

    Extends ProcessRunningStatus with port information.
    Maintains flat field structure for API compatibility.
    """

    dev_server_port: int
    frontend_port: int
    backend_port: int
    api_prefix: str = DEFAULT_API_PREFIX

    @property
    def ports(self) -> PortsConfig:
        """Extract ports configuration from response."""
        return PortsConfig(
            dev_server_port=self.dev_server_port,
            frontend_port=self.frontend_port,
            backend_port=self.backend_port,
        )

    @classmethod
    def from_config(
        cls,
        config: DevServerConfig,
        status: ProcessRunningStatus,
    ) -> StatusResponse:
        """Create StatusResponse from config and running status."""
        return cls(
            dev_server_port=config.dev_server_port,
            frontend_port=config.frontend_port,
            backend_port=config.backend_port,
            api_prefix=config.api_prefix,
            frontend_running=status.frontend_running,
            backend_running=status.backend_running,
            openapi_running=status.openapi_running,
            frontend_exit_code=status.frontend_exit_code,
            backend_exit_code=status.backend_exit_code,
        )


class PortsResponse(BaseModel):
    """Response model for ports endpoint.

    Maintains flat field structure for API compatibility.
    """

    dev_server_port: int
    frontend_port: int
    backend_port: int
    host: str
    api_prefix: str = DEFAULT_API_PREFIX

    @property
    def ports(self) -> PortsConfig:
        """Extract ports configuration from response."""
        return PortsConfig(
            dev_server_port=self.dev_server_port,
            frontend_port=self.frontend_port,
            backend_port=self.backend_port,
        )

    @classmethod
    def from_config(cls, config: DevServerConfig) -> PortsResponse:
        """Create PortsResponse from DevServerConfig."""
        return cls(
            dev_server_port=config.dev_server_port,
            frontend_port=config.frontend_port,
            backend_port=config.backend_port,
            host=config.host,
            api_prefix=config.api_prefix,
        )


# === MCP Response Models ===


class McpStatusResponse(ProcessRunningStatus):
    """MCP response model for status endpoint.

    Extends ProcessRunningStatus with optional port and PID information.
    Ports are optional because they may not be known when the server is not running.
    """

    dev_server_running: bool = False
    dev_server_port: int | None = None
    dev_server_pid: int | None = None
    frontend_port: int | None = None
    backend_port: int | None = None

    @classmethod
    def from_status_response(
        cls,
        status: StatusResponse,
        dev_server_running: bool,
        dev_server_port: int | None = None,
        dev_server_pid: int | None = None,
    ) -> McpStatusResponse:
        """Create from a StatusResponse with additional dev server info."""
        return cls(
            dev_server_running=dev_server_running,
            dev_server_port=dev_server_port,
            dev_server_pid=dev_server_pid,
            frontend_running=status.frontend_running,
            frontend_port=status.frontend_port,
            backend_running=status.backend_running,
            backend_port=status.backend_port,
            openapi_running=status.openapi_running,
            frontend_exit_code=status.frontend_exit_code,
            backend_exit_code=status.backend_exit_code,
        )

    @classmethod
    def not_running(cls) -> McpStatusResponse:
        """Create a response indicating nothing is running."""
        return cls(
            dev_server_running=False,
            frontend_running=False,
            backend_running=False,
            openapi_running=False,
        )


class McpMetadataResponse(BaseModel):
    """MCP response model for project metadata."""

    app_name: str
    app_module: str
    app_slug: str
    apx_version: str

    @classmethod
    def from_project_metadata(
        cls, metadata: ProjectMetadata, apx_version: str
    ) -> McpMetadataResponse:
        """Create from ProjectMetadata with version."""
        return cls(
            app_name=metadata.app_name,
            app_module=metadata.app_module,
            app_slug=metadata.app_slug,
            apx_version=apx_version,
        )


class McpErrorResponse(BaseModel):
    """MCP response model for errors."""

    error: str


class McpUrlResponse(BaseModel):
    """MCP response model for the frontend URL."""

    url: str


class McpDevLogsResponse(BaseModel):
    """MCP response model for dev logs snapshot."""

    logs: list[LogEntry]


class OpenApiStatusResponse(BaseModel):
    """Response model for OpenAPI regeneration status.

    Used by both dev server and MCP to track OpenAPI schema and api.ts regeneration times.
    """

    openapi_schema_path: str | None = None
    api_ts_path: str | None = None
    openapi_schema_last_updated: datetime | None = None
    api_ts_last_updated: datetime | None = None


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


class McpRouteCallResponse(BaseModel):
    """MCP response model for calling a backend route."""

    request_url: str
    method: HttpMethod
    status_code: int
    headers: dict[str, str]
    text: str | None = None
    # NOTE: Don't name this field "json" because it collides with BaseModel.json().
    json_body: JsonValue | None = None


class CheckCommandResult(CommandResult):
    """Result of running a single command as part of `apx dev check`.

    Extends CommandResult with a name field for identification.
    """

    name: str


class McpDevCheckResponse(BaseModel):
    """MCP response model for running `apx dev check` (structured)."""

    success: bool
    tsc: CheckCommandResult
    pyright: CheckCommandResult

    @property
    def all_passed(self) -> bool:
        """Check if all commands passed."""
        return self.tsc.returncode == 0 and self.pyright.returncode == 0


class McpDatabricksAppsLogsResponse(CommandResult):
    """MCP response model for `databricks apps logs` output.

    Extends CommandResult with app-specific metadata.
    """

    app_name: str
    resolved_from_databricks_yml: bool = False
