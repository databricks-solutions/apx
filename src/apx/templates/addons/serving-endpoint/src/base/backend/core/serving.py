"""Serving Endpoint client dependency."""

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.serving import ServingEndpointsAPI
from fastapi import Request


def get_serving_endpoint(request: Request) -> ServingEndpointsAPI:
    """Returns a Serving Endpoints API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.serving_endpoints
