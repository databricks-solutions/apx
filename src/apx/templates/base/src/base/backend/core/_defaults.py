from __future__ import annotations
from typing import Annotated, AsyncGenerator, TypeAlias
from contextlib import asynccontextmanager

from databricks.sdk import WorkspaceClient
from fastapi import Depends, FastAPI, Request

from ._base import LifespanDependency
from ._config import AppConfig, logger
from ._headers import HeadersDependency


class _ConfigDependency(LifespanDependency[AppConfig]):
    REGISTRY_NAME = "Config"

    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        app.state.config = AppConfig()
        logger.info(f"Starting app with configuration:\n{app.state.config}")
        yield

    @classmethod
    def get_instance(cls, request: Request) -> AppConfig:
        return request.app.state.config


class _WorkspaceClientDependency(LifespanDependency[WorkspaceClient]):
    @asynccontextmanager
    async def lifespan(self, app: FastAPI) -> AsyncGenerator[None, None]:
        app.state.workspace_client = WorkspaceClient()
        yield

    @classmethod
    def get_instance(cls, request: Request) -> WorkspaceClient:
        return request.app.state.workspace_client


def _get_user_ws(
    headers: HeadersDependency,
) -> WorkspaceClient:
    """
    Returns a Databricks Workspace client with authentication behalf of user.
    If the request contains an X-Forwarded-Access-Token header, on behalf of user authentication is used.

    Example usage: `user_ws: Dependencies.UserClient`
    """

    if not headers.token:
        raise ValueError(
            "OBO token is not provided in the header X-Forwarded-Access-Token"
        )

    return WorkspaceClient(
        token=headers.token.get_secret_value(), auth_type="pat"
    )  # set pat explicitly to avoid issues with SP client


ConfigDependency: TypeAlias = Annotated[AppConfig, Depends(_ConfigDependency)]

ClientDependency: TypeAlias = Annotated[
    WorkspaceClient, Depends(_WorkspaceClientDependency.get_instance)
]

UserWorkspaceClientDependency: TypeAlias = Annotated[
    WorkspaceClient, Depends(_get_user_ws)
]
