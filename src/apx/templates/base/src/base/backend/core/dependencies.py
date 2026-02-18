from __future__ import annotations

from typing import Annotated, TypeAlias

from databricks.sdk import WorkspaceClient
from fastapi import Depends, Header, Request

from ._config import AppConfig
from ._defaults import ConfigDependency, WorkspaceClientDependency  # noqa: F401 — triggers auto-registration
from ._headers import DatabricksAppsHeaders, get_databricks_headers


# --- Getters ---


def get_config(request: Request) -> AppConfig:
    """
    Returns the AppConfig instance from app.state.
    The config is initialized during application lifespan startup.
    """
    if not hasattr(request.app.state, "config"):
        raise RuntimeError(
            "AppConfig not initialized. "
            "Ensure app.state.config is set during application lifespan startup."
        )
    return request.app.state.config


def get_ws(request: Request) -> WorkspaceClient:
    """
    Returns the WorkspaceClient instance from app.state.
    The client is initialized during application lifespan startup.
    """
    if not hasattr(request.app.state, "workspace_client"):
        raise RuntimeError(
            "WorkspaceClient not initialized. "
            "Ensure app.state.workspace_client is set during application lifespan startup."
        )
    return request.app.state.workspace_client


def get_user_ws(
    token: Annotated[str | None, Header(alias="X-Forwarded-Access-Token")] = None,
) -> WorkspaceClient:
    """
    Returns a Databricks Workspace client with authentication behalf of user.
    If the request contains an X-Forwarded-Access-Token header, on behalf of user authentication is used.

    Example usage: `user_ws: Dependencies.UserClient`
    """

    if not token:
        raise ValueError(
            "OBO token is not provided in the header X-Forwarded-Access-Token"
        )

    return WorkspaceClient(
        token=token, auth_type="pat"
    )  # set pat explicitly to avoid issues with SP client


class Dependencies:
    """FastAPI dependency injection shorthand for route handler parameters."""

    Client: TypeAlias = Annotated[WorkspaceClient, Depends(get_ws)]
    """Databricks WorkspaceClient using app-level service principal credentials.
    Recommended usage: `ws: Dependencies.Client`"""

    UserClient: TypeAlias = Annotated[WorkspaceClient, Depends(get_user_ws)]
    """WorkspaceClient authenticated on behalf of the current user via OBO token.
    Requires the X-Forwarded-Access-Token header.
    Recommended usage: `user_ws: Dependencies.UserClient`"""

    Config: TypeAlias = Annotated[AppConfig, Depends(get_config)]
    """Application configuration loaded from environment variables.
    Recommended usage: `config: Dependencies.Config`"""

    Headers: TypeAlias = Annotated[
        DatabricksAppsHeaders, Depends(get_databricks_headers)
    ]
    """Databricks Apps HTTP headers for the current request.
    Recommended usage: `headers: Dependencies.Headers`"""
