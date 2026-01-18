"""Tests for the Rust MCP server implementation."""

from mcp.types import TextResourceContents

from pydantic import AnyUrl

import pytest
from mcp.client.session import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client


@pytest.mark.asyncio
async def test_apx_info_resource():
    """Test that the Rust MCP server provides the apx://info resource."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # List resources
            resources = await session.list_resources()
            uris = [str(r.uri) for r in resources.resources]
            assert "apx://info" in uris

            # Find the apx://info resource
            apx_info_resource = next(
                r for r in resources.resources if str(r.uri) == "apx://info"
            )
            assert apx_info_resource.name == "apx-info"
            desc = apx_info_resource.description
            assert desc is not None
            assert "apx toolkit" in desc.lower()
            assert apx_info_resource.mimeType == "text/plain"

            # Read the resource
            content = await session.read_resource(AnyUrl("apx://info"))
            assert len(content.contents) == 1
            contents = content.contents[0]
            assert isinstance(contents, TextResourceContents)
            text = contents.text
            assert text is not None
            assert "apx" in text.lower()
            assert "Databricks Apps" in text
            assert "Technology Stack" in text


@pytest.mark.asyncio
async def test_start_tool_exists():
    """Test that the start tool is available."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # List tools
            tools = await session.list_tools()
            tool_names = [t.name for t in tools.tools]
            assert "start" in tool_names

            # Find the start tool
            start_tool = next(t for t in tools.tools if t.name == "start")
            desc = start_tool.description
            assert desc is not None
            assert "start development server" in desc.lower()
            assert "inputSchema" in dir(start_tool)


@pytest.mark.asyncio
async def test_mcp_server_capabilities():
    """Test that the MCP server advertises correct capabilities."""
    server_params = StdioServerParameters(command="uv", args=["run", "apx", "mcp"])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            init_result = await session.initialize()

            # Check protocol version
            assert init_result.protocolVersion == "2024-11-05"

            # Check server info
            assert init_result.serverInfo is not None
            assert init_result.serverInfo.name == "apx"
            assert init_result.serverInfo.version is not None

            # Check capabilities
            assert init_result.capabilities is not None
            assert hasattr(init_result.capabilities, "resources")
            assert hasattr(init_result.capabilities, "tools")
