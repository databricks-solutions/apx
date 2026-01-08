"""MCP server instance.

This module contains the shared FastMCP server instance that all tools register with.
Separated into its own module to avoid circular imports.
"""

from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager

from mcp.server.fastmcp import FastMCP


@asynccontextmanager
async def lifespan(_app: FastMCP) -> AsyncGenerator[None, None]:
    """Lifespan context manager for the mcp app.

    This runs once during MCP server startup and handles initialization tasks.
    The actual initialization is done via the _initialize_mcp_sync() function which is
    called after all modules are loaded to avoid circular imports.

    Args:
        _app: The FastMCP application instance (unused but required by signature)
    """
    yield


# Initialize the shared MCP server instance
mcp = FastMCP("APX Dev Server", lifespan=lifespan)
