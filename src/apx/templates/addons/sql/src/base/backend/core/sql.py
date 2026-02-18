"""SQL Warehouse connection dependency."""

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.sql import StatementExecutionAPI
from fastapi import Request


def get_connection(request: Request) -> StatementExecutionAPI:
    """Returns a SQL Statement Execution API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.statement_execution
