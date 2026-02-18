from __future__ import annotations

from collections.abc import AsyncIterator, Callable
from contextlib import AbstractAsyncContextManager, asynccontextmanager

from databricks.sdk import WorkspaceClient
from fastapi import APIRouter, FastAPI

from ..._metadata import api_prefix, app_name, dist_dir
from .config import AppConfig, logger


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


@asynccontextmanager
async def _chain_lifespans(
    lifespans: list[Callable[[FastAPI], AbstractAsyncContextManager[None]]],
    app: FastAPI,
) -> AsyncIterator[None]:
    """Chain multiple lifespans into a single nested context manager."""
    if not lifespans:
        yield
        return

    head, *tail = lifespans

    async with head(app):
        async with _chain_lifespans(tail, app):
            yield


# --- Factory ---


def create_app(
    *,
    routers: list[APIRouter] | None = None,
    lifespans: list[
        Callable[[FastAPI], AbstractAsyncContextManager[None]]
    ]
    | None = None,
) -> FastAPI:
    """Create and configure a FastAPI application.

    Args:
        routers: List of APIRouter instances to include in the app.
        lifespans: Optional list of async context managers for custom startup/shutdown logic.
                  When provided, `app.state.config` and `app.state.workspace_client`
                  are already available.

    Returns:
        Configured FastAPI application instance.
    """

    @asynccontextmanager
    async def _composed_lifespan(app: FastAPI):
        async with _default_lifespan(app):
            async with _chain_lifespans(lifespans or [], app):
                yield

    app = FastAPI(title=app_name, lifespan=_composed_lifespan)

    for router in routers or []:
        app.include_router(router)

    if dist_dir.exists():
        from .static import CachedStaticFiles, add_not_found_handler

        app.mount("/", CachedStaticFiles(directory=dist_dir, html=True))
        add_not_found_handler(app)

    return app


def create_router() -> APIRouter:
    """Create an APIRouter with the application's API prefix."""
    return APIRouter(prefix=api_prefix)
