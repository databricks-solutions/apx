"""
Core application infrastructure: config, logging, utilities, dependencies, and bootstrap.
"""

from __future__ import annotations

import logging
import os
from collections.abc import Callable
from contextlib import AbstractAsyncContextManager, asynccontextmanager
from importlib import resources
from pathlib import Path
from typing import Annotated, ClassVar, TypeAlias

from databricks.sdk import WorkspaceClient
from dotenv import load_dotenv
from fastapi import APIRouter, Depends, FastAPI, Header, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict
from starlette.datastructures import Headers
from starlette.exceptions import HTTPException as StarletteHTTPException
from starlette.responses import Response
from starlette.staticfiles import NotModifiedResponse, StaticFiles
from starlette.types import Scope

from .._metadata import api_prefix, app_name, app_slug, dist_dir

# --- Config ---

project_root = Path(__file__).parent.parent.parent.parent
env_file = project_root / ".env"

if env_file.exists():
    load_dotenv(dotenv_path=env_file)


class AppConfig(BaseSettings):
    model_config: ClassVar[SettingsConfigDict] = SettingsConfigDict(
        env_file=env_file,
        env_prefix=f"{app_slug.upper()}_",
        extra="ignore",
        env_nested_delimiter="__",
    )
    app_name: str = Field(default=app_name)

    @property
    def static_assets_path(self) -> Path:
        return Path(str(resources.files(app_slug))).joinpath("__dist__")

    def __hash__(self) -> int:
        return hash(self.app_name)


# --- Logger ---

logger = logging.getLogger(app_name)


# --- Static Files ---


class CachedStaticFiles(StaticFiles):
    """StaticFiles with proper Cache-Control headers for SPA deployments.

    - Hashed assets (Vite `assets/` dir): cached immutably (hash changes on every build).
    - Everything else (index.html, etc.): `no-cache` — always revalidate via ETag/304.
    """

    def file_response(
        self,
        full_path: str | os.PathLike[str],
        stat_result: os.stat_result,
        scope: Scope,
        status_code: int = 200,
    ) -> Response:
        request_headers = Headers(scope=scope)
        response = FileResponse(
            full_path, status_code=status_code, stat_result=stat_result
        )

        if "/assets/" in str(full_path):
            response.headers["cache-control"] = "public, max-age=31536000, immutable"
        else:
            response.headers["cache-control"] = "no-cache"

        if self.is_not_modified(response.headers, request_headers):
            return NotModifiedResponse(response.headers)
        return response


# --- Utils ---


def _add_not_found_handler(app: FastAPI) -> None:
    """Register a handler that serves the SPA index.html for non-API 404s."""

    async def http_exception_handler(request: Request, exc: StarletteHTTPException):
        logger.info(
            f"HTTP exception handler called for request {request.url.path} with status code {exc.status_code}"
        )
        if exc.status_code == 404:
            path = request.url.path
            accept = request.headers.get("accept", "")

            is_api = path.startswith(api_prefix)
            is_get_page_nav = request.method == "GET" and "text/html" in accept

            # Heuristic: if the last path segment looks like a file (has a dot), don't SPA-fallback
            looks_like_asset = "." in path.split("/")[-1]

            if (not is_api) and is_get_page_nav and (not looks_like_asset):
                # Let the SPA router handle it
                return FileResponse(dist_dir / "index.html")
        # Default: return the original HTTP error (JSON 404 for API, etc.)
        return JSONResponse({"detail": exc.detail}, status_code=exc.status_code)

    app.exception_handler(StarletteHTTPException)(http_exception_handler)


# --- Lifespan ---


@asynccontextmanager
async def _default_lifespan(app: FastAPI):
    """Default lifespan that initializes config and workspace client."""
    config = AppConfig()
    logger.info(f"Starting app with configuration:\n{config}")
    ws = WorkspaceClient()

    app.state.config = config
    app.state.workspace_client = ws

    yield


# --- Factory ---


def create_app(
    *,
    routers: list[APIRouter] | None = None,
    lifespan: Callable[[FastAPI], AbstractAsyncContextManager[None]] | None = None,
) -> FastAPI:
    """Create and configure a FastAPI application.

    Args:
        routers: List of APIRouter instances to include in the app.
        lifespan: Optional async context manager for custom startup/shutdown logic.
                  When provided, `app.state.config` and `app.state.workspace_client`
                  are already available.

    Returns:
        Configured FastAPI application instance.
    """

    @asynccontextmanager
    async def _composed_lifespan(app: FastAPI):
        async with _default_lifespan(app):
            if lifespan:
                async with lifespan(app):
                    yield
            else:
                yield

    app = FastAPI(title=app_name, lifespan=_composed_lifespan)

    for router in routers or []:
        app.include_router(router)

    app.mount("/", CachedStaticFiles(directory=dist_dir, html=True))
    _add_not_found_handler(app)

    return app


def create_router() -> APIRouter:
    """Create an APIRouter with the application's API prefix."""
    return APIRouter(prefix=api_prefix)


# --- Dependencies ---


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

    Example usage: `user_ws: Dependency.UserClient`
    """

    if not token:
        raise ValueError(
            "OBO token is not provided in the header X-Forwarded-Access-Token"
        )

    return WorkspaceClient(
        token=token, auth_type="pat"
    )  # set pat explicitly to avoid issues with SP client


class Dependency:
    """FastAPI dependency injection shorthand for route handler parameters."""

    Client: TypeAlias = Annotated[WorkspaceClient, Depends(get_ws)]
    """Databricks WorkspaceClient using app-level service principal credentials.
    Recommended usage: `ws: Dependency.Client`"""

    UserClient: TypeAlias = Annotated[WorkspaceClient, Depends(get_user_ws)]
    """WorkspaceClient authenticated on behalf of the current user via OBO token.
    Requires the X-Forwarded-Access-Token header.
    Recommended usage: `user_ws: Dependency.UserClient`"""

    Config: TypeAlias = Annotated[AppConfig, Depends(get_config)]
    """Application configuration loaded from environment variables.
    Recommended usage: `config: Dependency.Config`"""
