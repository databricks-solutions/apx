"""Tests for the MCP server implementation."""

from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

import pytest

from apx.mcp.common import (
    McpSimpleStatusResponse,
    databricks_apps_logs,
    get_metadata,
    restart,
    start,
    status,
    stop,
)
from apx.models import (
    ActionResponse,
    DevConfig,
    McpActionResponse,
    McpDatabricksAppsLogsResponse,
    McpErrorResponse,
    McpMetadataResponse,
    ProjectConfig,
    ProjectMetadata,
    StatusResponse,
)


@pytest.fixture
def mock_project_config():
    """Create a mock project configuration."""
    config = ProjectConfig()
    config.dev = DevConfig()
    return config


@pytest.fixture
def mock_status_response():
    """Create a mock status response."""
    return StatusResponse(
        frontend_running=True,
        backend_running=True,
        openapi_running=True,
        dev_server_port=9000,
        frontend_port=5000,
        backend_port=8000,
    )


@pytest.fixture
def mock_core(mock_project_config):
    """Create a mock DevCore."""
    core = MagicMock()
    core.is_running = Mock(return_value=True)
    core.get_or_create_config = Mock(return_value=mock_project_config)
    return core


@pytest.fixture
def mock_client(mock_status_response):
    """Create a mock DevServerClient."""
    client = MagicMock()
    client.restart = Mock(
        return_value=ActionResponse(status="success", message="Restarted successfully")
    )
    client.status = Mock(return_value=mock_status_response)
    return client


# Helper to create a fake subprocess for testing CLI-based MCP tools
class FakeProcess:
    def __init__(self, returncode: int = 0, stdout: bytes = b"", stderr: bytes = b""):
        self.returncode = returncode
        self._stdout = stdout
        self._stderr = stderr

    async def communicate(self):
        return self._stdout, self._stderr


@pytest.mark.asyncio
async def test_start_success():
    """Test the start tool with successful server startup via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=0, stdout=b"Started successfully")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await start(
            host="localhost",
            obo=True,
            openapi=True,
            max_retries=10,
        )

        # Verify the result is a McpActionResponse
        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers started successfully" in result.message


@pytest.mark.asyncio
async def test_start_failure():
    """Test the start tool when server startup fails via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=1, stdout=b"", stderr=b"Port already in use")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await start()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Port already in use" in result.message


@pytest.mark.asyncio
async def test_restart_success():
    """Test the restart tool with successful restart via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=0, stdout=b"Restarted successfully")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers restarted successfully" in result.message


@pytest.mark.asyncio
async def test_restart_failure():
    """Test the restart tool when restart fails via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=1, stderr=b"Connection refused")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Connection refused" in result.message


@pytest.mark.asyncio
async def test_stop_success():
    """Test the stop tool with successful stop via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=0, stdout=b"Stopped successfully")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await stop()

        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers stopped successfully" in result.message


@pytest.mark.asyncio
async def test_stop_failure():
    """Test the stop tool when stop fails via subprocess."""

    async def mock_create_subprocess_exec(*args, **kwargs):
        return FakeProcess(returncode=1, stderr=b"Permission denied")

    with (
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_create_subprocess_exec,
        ),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await stop()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Permission denied" in result.message


@pytest.mark.asyncio
async def test_status_all_running(mock_core, mock_client, mock_status_response):
    """Test the status tool when all servers are running."""
    # Mock _get_ports to return PortsResponse
    from apx.models import PortsResponse

    def mock_get_ports(client):
        return PortsResponse(
            dev_server_port=9000,
            frontend_port=5173,
            backend_port=8000,
            host="localhost",
            api_prefix="/api",
        )

    # Get config with dev_server_port set
    mock_config = ProjectConfig()
    mock_config.dev.dev_server_port = 9000
    mock_core.get_or_create_config = Mock(return_value=mock_config)

    with (
        patch("apx.mcp.common._get_core", return_value=mock_core),
        patch("apx.mcp.common.DevServerClient", return_value=mock_client),
        patch("apx.mcp.common._get_ports", side_effect=mock_get_ports),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpSimpleStatusResponse)
        assert result.dev_server_running is True
        assert result.dev_server_url == "http://localhost:9000"
        assert result.api_prefix == "/api"
        assert result.frontend_running is True
        assert result.backend_running is True
        assert result.openapi_running is True

        # Verify client.status was called
        mock_client.status.assert_called_once()


@pytest.mark.asyncio
async def test_status_no_server():
    """Test the status tool when no server is configured."""
    core = MagicMock()
    core.is_running = Mock(return_value=False)

    with (
        patch("apx.mcp.common._get_core", return_value=core),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpSimpleStatusResponse)
        assert result.dev_server_running is False
        assert result.dev_server_url is None
        assert result.api_prefix is None
        assert result.frontend_running is False
        assert result.backend_running is False
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_status_server_not_running(mock_core):
    """Test the status tool when server process is not running."""
    mock_core.is_running.return_value = False

    with (
        patch("apx.mcp.common._get_core", return_value=mock_core),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpSimpleStatusResponse)
        assert result.dev_server_running is False
        assert result.frontend_running is False
        assert result.backend_running is False
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_status_client_error(mock_core, mock_client):
    """Test the status tool when client connection fails."""

    # Mock _get_ports to fail
    def mock_get_ports_fail(client):
        raise Exception("Connection refused")

    # Get config with dev_server_port set
    mock_config = ProjectConfig()
    mock_config.dev.dev_server_port = 9000
    mock_core.get_or_create_config = Mock(return_value=mock_config)

    with (
        patch("apx.mcp.common._get_core", return_value=mock_core),
        patch("apx.mcp.common.DevServerClient", return_value=mock_client),
        patch("apx.mcp.common._get_ports", side_effect=mock_get_ports_fail),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        # Should still return server info even if client fails
        assert isinstance(result, McpSimpleStatusResponse)
        assert result.dev_server_running is True
        assert result.dev_server_url == "http://localhost:9000"
        # But process statuses should be False
        assert result.frontend_running is False
        assert result.backend_running is False
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_get_metadata_success():
    """Test the get_metadata tool with successful metadata retrieval."""
    # Use field aliases as defined in ProjectMetadata model
    mock_metadata = ProjectMetadata(
        **{
            "app-name": "Test App",
            "app-module": "test_app",
            "app-slug": "test-app",
        }
    )

    with (
        patch("apx.mcp.common.ProjectMetadata.read", return_value=mock_metadata),
        patch("apx.mcp.common.apx_version", "1.0.0"),
    ):
        result = await get_metadata()

        assert isinstance(result, McpMetadataResponse)
        assert result.app_name == "Test App"
        assert result.app_module == "test_app"
        assert result.app_slug == "test-app"
        assert result.apx_version == "1.0.0"


@pytest.mark.asyncio
async def test_get_metadata_failure():
    """Test the get_metadata tool when metadata retrieval fails."""
    with (
        patch(
            "apx.mcp.common.ProjectMetadata.read",
            side_effect=Exception("pyproject.toml not found"),
        ),
    ):
        result = await get_metadata()

        assert isinstance(result, McpErrorResponse)
        assert "pyproject.toml not found" in result.error


@pytest.mark.asyncio
async def test_status_with_mocked_response(mock_core, mock_status_response):
    """Test status tool with a specific mocked status response."""
    from apx.models import PortsResponse

    # Customize the mock response
    custom_response = StatusResponse(
        frontend_running=False,
        backend_running=True,
        openapi_running=False,
        dev_server_port=9000,
        frontend_port=3000,
        backend_port=8080,
    )

    client = MagicMock()
    client.status = Mock(return_value=custom_response)

    def mock_get_ports(client):
        return PortsResponse(
            dev_server_port=9000,
            frontend_port=3000,
            backend_port=8080,
            host="localhost",
            api_prefix="/api",
        )

    # Get config with dev_server_port set
    mock_config = ProjectConfig()
    mock_config.dev.dev_server_port = 9000
    mock_core.get_or_create_config = Mock(return_value=mock_config)

    with (
        patch("apx.mcp.common._get_core", return_value=mock_core),
        patch("apx.mcp.common.DevServerClient", return_value=client),
        patch("apx.mcp.common._get_ports", side_effect=mock_get_ports),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpSimpleStatusResponse)
        assert result.dev_server_running is True
        assert result.frontend_running is False
        assert result.backend_running is True
        assert result.openapi_running is False
        assert result.dev_server_url == "http://localhost:9000"
        assert result.api_prefix == "/api"


@pytest.mark.asyncio
async def test_mcp_tool_responses_are_valid_models(mock_core):
    """Test that MCP tool responses are valid Pydantic models."""
    from apx.models import PortsResponse

    # Get config with dev_server_port set
    mock_config = ProjectConfig()
    mock_config.dev.dev_server_port = 9000
    mock_core.get_or_create_config = Mock(return_value=mock_config)

    client = MagicMock()
    client.status = Mock(
        return_value=StatusResponse(
            frontend_running=True,
            backend_running=True,
            openapi_running=True,
            dev_server_port=9000,
            frontend_port=5000,
            backend_port=8000,
        )
    )
    client.restart = Mock(
        return_value=ActionResponse(status="success", message="Restarted")
    )

    def mock_get_ports(client):
        return PortsResponse(
            dev_server_port=9000,
            frontend_port=5000,
            backend_port=8000,
            host="localhost",
            api_prefix="/api",
        )

    async def mock_subprocess_success(*args, **kwargs):
        return FakeProcess(returncode=0)

    with (
        patch("apx.mcp.common._get_core", return_value=mock_core),
        patch("apx.mcp.common.DevServerClient", return_value=client),
        patch("apx.mcp.common._get_ports", side_effect=mock_get_ports),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
        patch(
            "apx.mcp.common.ProjectMetadata.read",
            return_value=ProjectMetadata(
                **{
                    "app-name": "Test App",
                    "app-module": "test_app",
                    "app-slug": "test-app",
                }
            ),
        ),
        patch("apx.mcp.common.apx_version", "1.0.0"),
        patch(
            "apx.mcp.common.asyncio.create_subprocess_exec",
            side_effect=mock_subprocess_success,
        ),
    ):
        # Test start response
        start_result = await start()
        assert isinstance(start_result, McpActionResponse)
        assert start_result.model_dump()  # Should serialize to dict

        # Test status response
        status_result = await status()
        assert isinstance(status_result, McpSimpleStatusResponse)
        assert status_result.model_dump()  # Should serialize to dict

        # Test get_metadata response
        metadata_result = await get_metadata()
        assert isinstance(metadata_result, McpMetadataResponse)
        assert metadata_result.model_dump()  # Should serialize to dict

        # Test restart response
        restart_result = await restart()
        assert isinstance(restart_result, McpActionResponse)
        assert restart_result.model_dump()  # Should serialize to dict

        # Test stop response
        stop_result = await stop()
        assert isinstance(stop_result, McpActionResponse)
        assert stop_result.model_dump()  # Should serialize to dict


@pytest.mark.asyncio
async def test_databricks_apps_logs_with_explicit_app_name(
    monkeypatch: pytest.MonkeyPatch,
):
    class FakeProc:
        def __init__(self):
            self.returncode = 0

        async def communicate(self):
            return b"hello\n", b""

        def kill(self):
            return None

    async def fake_create_subprocess_exec(*args, **kwargs):
        return FakeProc()

    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )

    result = await databricks_apps_logs(app_name="my-app", tail_lines=10)
    assert isinstance(result, McpDatabricksAppsLogsResponse)
    assert result.app_name == "my-app"
    assert result.resolved_from_databricks_yml is False
    assert "databricks" in result.command[0]
    assert result.returncode == 0
    assert "hello" in result.stdout


@pytest.mark.asyncio
async def test_databricks_apps_logs_resolves_app_from_databricks_yml(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    (tmp_path / "databricks.yml").write_text(
        """
resources:
  apps:
    demo-app:
      name: "resolved-app"
""".lstrip()
    )

    class FakeProc:
        def __init__(self):
            self.returncode = 0

        async def communicate(self):
            return b"resolved logs\n", b""

        def kill(self):
            return None

    async def fake_create_subprocess_exec(*args, **kwargs):
        return FakeProc()

    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )
    monkeypatch.setattr("apx.mcp.common.Path.cwd", lambda: tmp_path)

    result = await databricks_apps_logs(app_name=None)
    assert isinstance(result, McpDatabricksAppsLogsResponse)
    assert result.app_name == "resolved-app"
    assert result.resolved_from_databricks_yml is True
    assert "resolved logs" in result.stdout


@pytest.mark.asyncio
async def test_databricks_apps_logs_errors_when_multiple_apps_in_yml(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    (tmp_path / "databricks.yml").write_text(
        """
resources:
  apps:
    a1:
      name: "app-1"
    a2:
      name: "app-2"
""".lstrip()
    )

    monkeypatch.setattr("apx.mcp.common.Path.cwd", lambda: tmp_path)

    result = await databricks_apps_logs(app_name=None)
    assert isinstance(result, McpErrorResponse)
    assert "multiple apps" in result.error.lower()


@pytest.mark.asyncio
async def test_databricks_apps_logs_errors_when_databricks_yml_missing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr("apx.mcp.common.Path.cwd", lambda: tmp_path)
    result = await databricks_apps_logs(app_name=None)
    assert isinstance(result, McpErrorResponse)
    assert "databricks.yml was not found" in result.error


@pytest.mark.asyncio
async def test_databricks_apps_logs_logs_subcommand_not_found_upgrade_message(
    monkeypatch: pytest.MonkeyPatch,
):
    class FakeProc:
        def __init__(self):
            self.returncode = 1

        async def communicate(self):
            return b"", b'Error: unknown command "logs" for "apps"\\n'

        def kill(self):
            return None

    async def fake_create_subprocess_exec(*args, **kwargs):
        return FakeProc()

    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )

    result = await databricks_apps_logs(app_name="my-app")
    assert isinstance(result, McpErrorResponse)
    assert "upgrade Databricks CLI to v0.280.0 or higher" in result.error


@pytest.mark.asyncio
async def test_databricks_apps_logs_forwards_other_cli_errors(
    monkeypatch: pytest.MonkeyPatch,
):
    class FakeProc:
        def __init__(self):
            self.returncode = 2

        async def communicate(self):
            return b"some stdout", b"some stderr"

        def kill(self):
            return None

    async def fake_create_subprocess_exec(*args, **kwargs):
        return FakeProc()

    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )

    result = await databricks_apps_logs(app_name="my-app")
    assert isinstance(result, McpErrorResponse)
    assert "some stderr" in result.error
    assert "some stdout" in result.error


@pytest.mark.asyncio
async def test_databricks_apps_logs_errors_when_databricks_cli_missing(
    monkeypatch: pytest.MonkeyPatch,
):
    async def fake_create_subprocess_exec(*args, **kwargs):
        raise FileNotFoundError("databricks")

    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )

    result = await databricks_apps_logs(app_name="my-app")
    assert isinstance(result, McpErrorResponse)
    assert "Databricks CLI executable not found" in result.error


@pytest.mark.asyncio
async def test_databricks_apps_logs_loads_dotenv_when_present(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    (tmp_path / ".env").write_text("DATABRICKS_CONFIG_PROFILE=DEFAULT\n")
    monkeypatch.setattr("apx.mcp.common.Path.cwd", lambda: tmp_path)

    called = {"val": False}

    def fake_load_dotenv(path, *args, **kwargs):
        # Ensure we load the expected file
        assert str(path).endswith(str(tmp_path / ".env"))
        called["val"] = True
        return True

    class FakeProc:
        def __init__(self):
            self.returncode = 0

        async def communicate(self):
            return b"ok\n", b""

        def kill(self):
            return None

    async def fake_create_subprocess_exec(*args, **kwargs):
        return FakeProc()

    monkeypatch.setattr("apx.mcp.common.load_dotenv", fake_load_dotenv)
    monkeypatch.setattr(
        "apx.mcp.common.asyncio.create_subprocess_exec", fake_create_subprocess_exec
    )

    result = await databricks_apps_logs(app_name="my-app")
    assert isinstance(result, McpDatabricksAppsLogsResponse)
    assert called["val"] is True
