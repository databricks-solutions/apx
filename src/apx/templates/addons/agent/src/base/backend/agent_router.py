"""Agent tools — replace with your own.

Each function becomes a tool: type hints define the schema, the docstring
is the description. Dependencies.* parameters are injected by FastAPI and
excluded from the tool schema.
"""

from .core import Dependencies
from .core.agent import Agent


def list_catalogs(ws: Dependencies.UserClient) -> list[str]:
    """List Unity Catalog catalogs accessible to the current user."""
    return [c.name for c in ws.catalogs.list() if c.name]


agent = Agent(tools=[list_catalogs])
