"""MCP server package for apx.

This package provides MCP (Model Context Protocol) tools for:
- Development server management (start, stop, restart, status)
- Databricks SDK documentation search and lookup
"""

# Import mcp from server module (no circular import)
from apx.mcp.server import mcp

# Import tools to register them with the mcp instance
# These imports must come after mcp is available
from apx.mcp import common as _common  # noqa: F401, E402
from apx.mcp import sdk as _sdk  # noqa: F401, E402


def run_mcp_server() -> None:
    """Run the MCP server using stdio transport."""
    mcp.run()


__all__ = ["mcp", "run_mcp_server"]
