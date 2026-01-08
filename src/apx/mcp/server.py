"""MCP server instance.

This module contains the shared FastMCP server instance that all tools register with.
Separated into its own module to avoid circular imports.
"""

from mcp.server.fastmcp import FastMCP

# Initialize the shared MCP server instance
mcp = FastMCP("APX Dev Server")
