"""MCP server package for apx.

This package provides MCP (Model Context Protocol) tools for:
- Development server management (start, stop, restart, status)
- Databricks SDK documentation search and lookup
"""

# Import mcp from server module (no circular import)
from apx.mcp.server import mcp

# Import tools to register them with the mcp instance
# These imports must come after mcp is available
from apx.mcp import common as _common  # noqa: F401
from apx.mcp import sdk as _sdk  # noqa: F401

# Track initialization state
_initialized = False


def _initialize_mcp_sync() -> None:
    """Initialize MCP server components synchronously.

    This is called once during server startup to handle optional initialization tasks:
    - Downloads and caches the Databricks SDK repository (if not cached)
    - Prepares the SDK documentation search index

    All initialization is optional and won't fail the server startup.
    """
    global _initialized

    if _initialized:
        return

    # Import here after all modules are loaded
    from apx.mcp.sdk import initialize_sdk_cache

    # Initialize SDK cache (optional, won't fail)
    try:
        initialize_sdk_cache()
    except Exception:
        # Silently skip on any error
        pass

    _initialized = True


def run_mcp_server() -> None:
    """Run the MCP server using stdio transport."""
    # Initialize components before running
    _initialize_mcp_sync()
    mcp.run()


__all__ = ["mcp", "run_mcp_server"]
