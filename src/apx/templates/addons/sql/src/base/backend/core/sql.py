"""SQL Warehouse connection, query execution addon, and routes."""

from __future__ import annotations

from typing import Annotated

from databricks.sdk.service.sql import (
    Disposition,
    Format,
    StatementExecutionAPI,
    StatementResponse,
)
from fastapi import APIRouter, Depends, FastAPI, Request, Response
from pydantic import BaseModel, Field
from pydantic_settings import SettingsConfigDict
from databricks.sdk import WorkspaceClient

from ._base import AddonConfig, Dependency


class SqlConfig(AddonConfig):
    model_config = SettingsConfigDict(env_prefix="DATABRICKS_SQL_")
    warehouse_id: str = Field(description="SQL Warehouse ID")


# --- Addon dependency ---


class SqlDependency(Dependency[StatementExecutionAPI]):
    REGISTRY_NAME = "Sql"

    async def initialize(self, app: FastAPI) -> None:
        self.config = SqlConfig()  # ty: ignore[missing-argument]
        app.state.sql_config = self.config

    async def shutdown(self, app: FastAPI) -> None:
        pass

    def get_routers(self) -> list[APIRouter]:
        return [query_router]

# --- FastAPI dependencies ---


def get_sql_config(request: Request) -> SqlConfig:
    """Returns the SQL Warehouse config from app state."""
    return request.app.state.sql_config


def get_sql(
    request: Request, user_ws: Annotated[WorkspaceClient, Depends(get_user_ws)]
) -> StatementExecutionAPI:
    """FastAPI dependency -- returns a StatementExecutionAPI for the current user."""
    return user_ws.statement_execution


# --- Routes ---

query_router = APIRouter(prefix="/query")


class ExecuteQueryRequest(BaseModel):
    statement: str = Field(description="SQL statement to execute (max 16 MiB)")
    warehouse_id: str | None = Field(
        default=None, description="Override warehouse ID from config"
    )
    catalog: str | None = Field(default=None, description="Default catalog")
    schema_name: str | None = Field(
        default=None, alias="schema", description="Default schema"
    )
    wait_timeout: str = Field(
        default="0s", description="'0s' for async, '5s'-'50s' for sync"
    )
    disposition: Disposition = Field(default=Disposition.INLINE)
    format: Format = Field(default=Format.JSON_ARRAY)
    byte_limit: int | None = Field(default=None, description="Result byte size limit")
    row_limit: int | None = Field(default=None, description="Result row count limit")


@query_router.post("", operation_id="executeQuery", response_model=StatementResponse)
def execute_query(
    body: ExecuteQueryRequest,
    sql: Annotated[StatementExecutionAPI, Depends(get_sql)],
    config: Annotated[SqlConfig, Depends(get_sql_config)],
) -> StatementResponse:
    """Execute a SQL statement. Defaults to async (wait_timeout='0s')."""
    return sql.execute_statement(
        statement=body.statement,
        warehouse_id=body.warehouse_id or config.warehouse_id,
        wait_timeout=body.wait_timeout,
        disposition=body.disposition,
        format=body.format,
        catalog=body.catalog,
        schema=body.schema_name,
        byte_limit=body.byte_limit,
        row_limit=body.row_limit,
    )


@query_router.get(
    "/{statement_id}", operation_id="getStatement", response_model=StatementResponse
)
def get_statement(
    statement_id: str,
    sql: Annotated[StatementExecutionAPI, Depends(get_sql)],
) -> StatementResponse:
    """Get statement status, manifest, and result data."""
    return sql.get_statement(statement_id)


@query_router.post(
    "/{statement_id}/cancel", operation_id="cancelQuery", status_code=204
)
def cancel_query(
    statement_id: str,
    sql: Annotated[StatementExecutionAPI, Depends(get_sql)],
):
    """Cancel a running statement execution."""
    sql.cancel_execution(statement_id)
    return Response(status_code=204)



