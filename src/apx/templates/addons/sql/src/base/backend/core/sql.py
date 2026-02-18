"""SQL Warehouse connection dependency."""
# /// apx
# exports = ["from .sql import SqlDependency, get_connection"]
# imports = [
#     "from .sql import get_connection",
#     "from databricks.sdk.service.sql import StatementExecutionAPI",
# ]
# aliases = ["Connection: TypeAlias = Annotated[StatementExecutionAPI, Depends(get_connection)]"]
# ///

from __future__ import annotations

from databricks.sdk import WorkspaceClient
from databricks.sdk.service.sql import StatementExecutionAPI
from fastapi import FastAPI, Request
from pydantic import Field
from pydantic_settings import SettingsConfigDict

from ._base import AddonConfig, Dependency


class SqlDependency(Dependency[StatementExecutionAPI]):
    REGISTRY_NAME = "Connection"

    class Config(AddonConfig):
        model_config = SettingsConfigDict(env_prefix="DATABRICKS_SQL_")
        warehouse_id: str = Field(description="SQL Warehouse ID")

    async def initialize(self, app: FastAPI) -> None:
        self.config = self.Config()  # ty: ignore[missing-argument]
        app.state.sql_config = self.config

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_instance(self, request: Request) -> StatementExecutionAPI:
        ws: WorkspaceClient = request.app.state.workspace_client
        return ws.statement_execution


def get_connection(request: Request) -> StatementExecutionAPI:
    """Returns a SQL Statement Execution API client from the workspace client."""
    ws: WorkspaceClient = request.app.state.workspace_client
    return ws.statement_execution
