"""Genie Space client dependency."""

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.dashboards import GenieAPI
from fastapi import Request


def get_genie(request: Request) -> GenieAPI:
    """Returns a Genie API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.genie
