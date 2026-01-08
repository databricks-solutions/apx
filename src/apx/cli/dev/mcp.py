"""MCP server implementation for apx dev commands.

This module re-exports MCP functionality from the apx.mcp package
for backwards compatibility.
"""

# Re-export everything needed for backwards compatibility
from apx.mcp import mcp, run_mcp_server
from apx.mcp.common import McpSimpleStatusResponse

# Re-export tools for testing imports
from apx.mcp.common import (
    databricks_apps_logs,
    get_metadata,
    restart,
    start,
    status,
    stop,
)

__all__ = [
    "mcp",
    "run_mcp_server",
    "McpSimpleStatusResponse",
    "start",
    "restart",
    "stop",
    "status",
    "get_metadata",
    "databricks_apps_logs",
]
