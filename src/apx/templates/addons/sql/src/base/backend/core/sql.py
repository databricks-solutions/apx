"""SQL Warehouse connection, query execution addon, and routes."""

from __future__ import annotations

from contextlib import asynccontextmanager
from functools import partial
from typing import Annotated, AsyncGenerator, Callable, TypeAlias

from databricks.sdk.service.sql import (
    StatementExecutionAPI,
)
from fastapi import Depends, FastAPI, Request
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

    @property
    def execute_statement(self):
        """Partially apply the warehouse ID to the execute_statement method."""
        return partial(
            self.api.execute_statement, warehouse_id=self.config.warehouse_id
        )


class _SqlDependency(LifespanDependency):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        app.state.sql_config = SqlConfig()  # ty: ignore[missing-argument]
        yield

    @staticmethod
    def get_instance(user_ws: UserWorkspaceClientDependency, request: Request) -> Sql:
        return Sql(config=request.app.state.sql_config, api=user_ws.statement_execution)


SqlDependency: TypeAlias = Annotated[Sql, Depends(_SqlDependency.get_instance)]
