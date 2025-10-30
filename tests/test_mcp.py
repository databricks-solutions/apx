"""Tests for the MCP server implementation."""

import io
import sys
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

import pytest

from apx.cli.dev.mcp import (
    get_metadata,
    restart,
    start,
    status,
    stop,
)
from apx.cli.dev.models import (
    ActionResponse,
    DevConfig,
    McpActionResponse,
    McpErrorResponse,
    McpMetadataResponse,
    McpStatusResponse,
    ProjectConfig,
    StatusResponse,
)
from apx.utils import ProjectMetadata


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
        frontend_port=5173,
        backend_port=8000,
    )


@pytest.fixture
def mock_manager(mock_project_config):
    """Create a mock DevManager."""
    manager = MagicMock()
    manager.socket_path = Path("/test/project/.apx/dev.sock")
    manager.is_dev_server_running = Mock(return_value=True)
    manager.start = Mock()
    manager.stop = Mock()
    return manager


@pytest.fixture
def mock_client(mock_status_response):
    """Create a mock DevServerClient."""
    client = MagicMock()
    client.restart = Mock(
        return_value=ActionResponse(status="success", message="Restarted successfully")
    )
    client.status = Mock(return_value=mock_status_response)
    return client


@pytest.mark.asyncio
async def test_start_success(mock_manager):
    """Test the start tool with successful server startup."""
    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await start(
            frontend_port=5173,
            backend_port=8000,
            host="localhost",
            obo=True,
            openapi=True,
            max_retries=10,
        )

        # Verify the result is a McpActionResponse
        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers started successfully" in result.message

        # Verify manager.start was called with correct parameters
        mock_manager.start.assert_called_once_with(
            frontend_port=5173,
            backend_port=8000,
            host="localhost",
            obo=True,
            openapi=True,
            max_retries=10,
            watch=False,
        )


@pytest.mark.asyncio
async def test_start_failure(mock_manager):
    """Test the start tool when server startup fails."""
    mock_manager.start.side_effect = Exception("Port already in use")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await start(frontend_port=5173, backend_port=8000)

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Port already in use" in result.message


@pytest.mark.asyncio
async def test_restart_success(mock_manager):
    """Test the restart tool with successful restart."""
    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers restarted successfully" in result.message

        # Verify manager methods were called
        mock_manager.stop.assert_called_once()
        mock_manager.start.assert_called_once()


@pytest.mark.asyncio
async def test_restart_no_server():
    """Test the restart tool when no server is running."""
    manager = MagicMock()
    manager.is_dev_server_running = Mock(return_value=False)
    manager.socket_path = Path("/test/project/.apx/dev.sock")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Development server is not running" in result.message


@pytest.mark.asyncio
async def test_restart_server_not_running():
    """Test the restart tool when server is not running."""
    manager = MagicMock()
    manager.is_dev_server_running = Mock(return_value=False)
    manager.socket_path = Path("/test/project/.apx/dev.sock")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Development server is not running" in result.message


@pytest.mark.asyncio
async def test_restart_failure(mock_manager):
    """Test the restart tool when restart fails."""
    mock_manager.start.side_effect = Exception("Connection refused")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await restart()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Connection refused" in result.message


@pytest.mark.asyncio
async def test_stop_success(mock_manager):
    """Test the stop tool with successful stop."""
    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await stop()

        assert isinstance(result, McpActionResponse)
        assert result.status == "success"
        assert "Development servers stopped successfully" in result.message

        # Verify manager.stop was called
        mock_manager.stop.assert_called_once()


@pytest.mark.asyncio
async def test_stop_failure(mock_manager):
    """Test the stop tool when stop fails."""
    mock_manager.stop.side_effect = Exception("Permission denied")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await stop()

        assert isinstance(result, McpActionResponse)
        assert result.status == "error"
        assert "Permission denied" in result.message


@pytest.mark.asyncio
async def test_status_all_running(mock_manager, mock_client, mock_status_response):
    """Test the status tool when all servers are running."""
    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("apx.cli.dev.mcp.DevServerClient", return_value=mock_client),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpStatusResponse)
        assert result.dev_server_running is True
        assert result.dev_server_port is None  # No port tracking with Unix sockets
        assert result.dev_server_pid is None  # No PID tracking with Unix sockets
        assert result.frontend_running is True
        assert result.frontend_port == 5173
        assert result.backend_running is True
        assert result.backend_port == 8000
        assert result.openapi_running is True

        # Verify client.status was called
        mock_client.status.assert_called_once()


@pytest.mark.asyncio
async def test_status_no_server():
    """Test the status tool when no server is configured."""
    manager = MagicMock()
    manager.is_dev_server_running = Mock(return_value=False)
    manager.socket_path = Path("/test/project/.apx/dev.sock")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpStatusResponse)
        assert result.dev_server_running is False
        assert result.dev_server_port is None
        assert result.dev_server_pid is None
        assert result.frontend_running is False
        assert result.backend_running is False
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_status_server_not_running(mock_manager):
    """Test the status tool when server process is not running."""
    mock_manager.is_dev_server_running.return_value = False

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpStatusResponse)
        assert result.dev_server_running is False
        assert result.frontend_running is False
        assert result.backend_running is False
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_status_client_error(mock_manager, mock_client):
    """Test the status tool when client connection fails."""
    mock_client.status.side_effect = Exception("Connection refused")

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("apx.cli.dev.mcp.DevServerClient", return_value=mock_client),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        # Should still return server info even if client fails
        assert isinstance(result, McpStatusResponse)
        assert result.dev_server_running is True
        assert result.dev_server_port is None  # No port tracking with Unix sockets
        assert result.dev_server_pid is None  # No PID tracking with Unix sockets
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
        patch("apx.cli.dev.mcp.get_project_metadata", return_value=mock_metadata),
        patch("apx.cli.dev.mcp.apx_version", "1.0.0"),
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
            "apx.cli.dev.mcp.get_project_metadata",
            side_effect=Exception("pyproject.toml not found"),
        ),
    ):
        result = await get_metadata()

        assert isinstance(result, McpErrorResponse)
        assert "pyproject.toml not found" in result.error


@pytest.mark.asyncio
async def test_status_with_mocked_response(mock_manager, mock_status_response):
    """Test status tool with a specific mocked status response."""
    # Customize the mock response
    custom_response = StatusResponse(
        frontend_running=False,
        backend_running=True,
        openapi_running=False,
        frontend_port=3000,
        backend_port=8080,
    )

    client = MagicMock()
    client.status = Mock(return_value=custom_response)

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
        patch("apx.cli.dev.mcp.DevServerClient", return_value=client),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
    ):
        result = await status()

        assert isinstance(result, McpStatusResponse)
        assert result.dev_server_running is True
        assert result.frontend_running is False
        assert result.frontend_port == 3000
        assert result.backend_running is True
        assert result.backend_port == 8080
        assert result.openapi_running is False


@pytest.mark.asyncio
async def test_start_suppresses_console_output(mock_manager):
    """Test that start tool suppresses console output when called."""
    original_stdout = sys.stdout
    captured_output = io.StringIO()

    try:
        sys.stdout = captured_output

        with (
            patch("apx.cli.dev.mcp._get_manager", return_value=mock_manager),
            patch("pathlib.Path.cwd", return_value=Path("/test/project")),
        ):
            result = await start(frontend_port=5173, backend_port=8000)

            # Verify the result is successful
            assert isinstance(result, McpActionResponse)
            assert result.status == "success"

            # Verify that console output was suppressed
            # The captured output should be empty or minimal
            output = captured_output.getvalue()
            # Console.print statements from manager.start() should not appear
            assert "🔍 Finding" not in output
            assert "✓ Found available ports" not in output
            assert "🚀 Starting" not in output
            assert "Dev Server:" not in output
    finally:
        sys.stdout = original_stdout


@pytest.mark.asyncio
async def test_mcp_tool_responses_are_valid_models():
    """Test that MCP tool responses are valid Pydantic models."""
    manager = MagicMock()
    manager.socket_path = Path("/test/project/.apx/dev.sock")
    manager.is_dev_server_running = Mock(return_value=True)
    manager.start = Mock()
    manager.stop = Mock()

    client = MagicMock()
    client.status = Mock(
        return_value=StatusResponse(
            frontend_running=True,
            backend_running=True,
            openapi_running=True,
            frontend_port=5173,
            backend_port=8000,
        )
    )
    client.restart = Mock(
        return_value=ActionResponse(status="success", message="Restarted")
    )

    with (
        patch("apx.cli.dev.mcp._get_manager", return_value=manager),
        patch("apx.cli.dev.mcp.DevServerClient", return_value=client),
        patch("pathlib.Path.cwd", return_value=Path("/test/project")),
        patch(
            "apx.cli.dev.mcp.get_project_metadata",
            return_value=ProjectMetadata(
                **{
                    "app-name": "Test App",
                    "app-module": "test_app",
                    "app-slug": "test-app",
                }
            ),
        ),
        patch("apx.cli.dev.mcp.apx_version", "1.0.0"),
    ):
        # Test start response
        start_result = await start()
        assert isinstance(start_result, McpActionResponse)
        assert start_result.model_dump()  # Should serialize to dict

        # Test status response
        status_result = await status()
        assert isinstance(status_result, McpStatusResponse)
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
