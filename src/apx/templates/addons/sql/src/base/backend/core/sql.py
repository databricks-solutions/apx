"""SQL Warehouse connection, query execution addon, and routes."""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import Annotated, AsyncGenerator, TypeAlias

from databricks.sdk.service.sql import (
    Disposition,
    Format,
    StatementExecutionAPI,
    StatementResponse,
)
from fastapi import APIRouter, Depends, FastAPI, Request, Response
from pydantic import BaseModel, Field
from pydantic_settings import BaseSettings, SettingsConfigDict

from ._base import LifespanDependency
from ._defaults import UserWorkspaceClientDependency


class SqlConfig(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="DATABRICKS_SQL_")
    warehouse_id: str = Field(description="SQL Warehouse ID")


# --- Addon dependency ---


class Sql(BaseModel):
    config: SqlConfig
    api: StatementExecutionAPI


class _SqlDependency(LifespanDependency[StatementExecutionAPI]):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        app.state.sql_config = SqlConfig()  # ty: ignore[missing-argument]
        yield

    def get_routers(self) -> list[APIRouter]:
        return [query_router]

    @classmethod
    def get_instance(
        cls, user_ws: UserWorkspaceClientDependency, request: Request
    ) -> Sql:
        return Sql(config=request.app.state.sql_config, api=user_ws.statement_execution)


SqlDependency: TypeAlias = Annotated[Sql, Depends(_SqlDependency.as_depends())]

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
    sql: SqlDependency,
) -> StatementResponse:
    """Execute a SQL statement. Defaults to async (wait_timeout='0s')."""
    return sql.api.execute_statement(
        statement=body.statement,
        warehouse_id=body.warehouse_id or sql.config.warehouse_id,
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
    sql: SqlDependency,
) -> StatementResponse:
    """Get statement status, manifest, and result data."""
    return sql.api.get_statement(statement_id)


@query_router.post(
    "/{statement_id}/cancel", operation_id="cancelQuery", status_code=204
)
def cancel_query(
    statement_id: str,
    sql: SqlDependency,
):
    """Cancel a running statement execution."""
    sql.api.cancel_execution(statement_id)
    return Response(status_code=204)
