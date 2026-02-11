"""
Core application infrastructure: config, logging, utilities, dependencies, and bootstrap.

Stateful variant -- includes database engine, session management, and DatabaseConfig.
"""

from __future__ import annotations

import logging
import os
from contextlib import asynccontextmanager
from importlib import resources
from pathlib import Path
from typing import Annotated, ClassVar, Generator, Optional, TypeAlias

from databricks.sdk import WorkspaceClient
from databricks.sdk.errors import NotFound
from dotenv import load_dotenv
from fastapi import Depends, FastAPI, Header, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict
from sqlalchemy import Engine, create_engine, event
from sqlmodel import Session, SQLModel, text
from starlette.exceptions import HTTPException as StarletteHTTPException

from .._metadata import api_prefix, app_name, app_slug, dist_dir

# --- Config ---

project_root = Path(__file__).parent.parent.parent.parent
env_file = project_root / ".env"

if env_file.exists():
    load_dotenv(dotenv_path=env_file)


class DatabaseConfig(BaseSettings):
    model_config: ClassVar[SettingsConfigDict] = SettingsConfigDict(
        extra="ignore",
    )
    port: int = Field(
        description="The port of the database", default=5432, validation_alias="PGPORT"
    )
    database_name: str = Field(
        description="The name of the database", default="databricks_postgres"
    )
    instance_name: str = Field(
        description="The name of the database instance", validation_alias="PGAPPNAME"
    )


class AppConfig(BaseSettings):
    model_config: ClassVar[SettingsConfigDict] = SettingsConfigDict(
        env_file=env_file,
        env_prefix=f"{app_slug.upper()}_",
        extra="ignore",
        env_nested_delimiter="__",
    )
    app_name: str = Field(default=app_name)
    db: DatabaseConfig = DatabaseConfig()  # type: ignore

    @property
    def static_assets_path(self) -> Path:
        return Path(str(resources.files(app_slug))).joinpath("__dist__")

    def __hash__(self) -> int:
        return hash(self.app_name)


# --- Logger ---

logger = logging.getLogger(app_name)


# --- Utils ---


def add_not_found_handler(app: FastAPI) -> None:
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


# --- Database ---


def _get_dev_db_port() -> int | None:
    """Check for APX_DEV_DB_PORT environment variable for local development."""
    port = os.environ.get("APX_DEV_DB_PORT")
    return int(port) if port else None


def _build_engine_url(
    config: AppConfig, ws: WorkspaceClient, dev_port: int | None
) -> str:
    """Build the database engine URL for dev or production mode."""
    if dev_port:
        logger.info(f"Using local dev database at localhost:{dev_port}")
        username = "postgres"
        password = os.environ.get("APX_DEV_DB_PWD")
        if password is None:
            raise ValueError(
                "APX server didn't provide a password, please check the dev server logs"
            )
        return f"postgresql+psycopg://{username}:{password}@localhost:{dev_port}/postgres?sslmode=disable"

    # Production mode: use Databricks Database
    logger.info(f"Using Databricks database instance: {config.db.instance_name}")
    instance = ws.database.get_database_instance(config.db.instance_name)
    prefix = "postgresql+psycopg"
    host = instance.read_write_dns
    port = config.db.port
    database = config.db.database_name
    username = (
        ws.config.client_id if ws.config.client_id else ws.current_user.me().user_name
    )
    return f"{prefix}://{username}:@{host}:{port}/{database}"


def create_db_engine(config: AppConfig, ws: WorkspaceClient) -> Engine:
    """
    Create a SQLAlchemy engine.

    In dev mode: no SSL, no password callback.
    In production: require SSL and use Databricks credential callback.
    """
    dev_port = _get_dev_db_port()
    engine_url = _build_engine_url(config, ws, dev_port)

    engine = create_engine(
        engine_url,
        pool_recycle=45 * 60 if not dev_port else None,
        connect_args={"sslmode": "require"} if not dev_port else None,
        pool_size=4,
    )

    def before_connect(dialect, conn_rec, cargs, cparams):
        cred = ws.database.generate_database_credential(
            instance_names=[config.db.instance_name]
        )
        cparams["password"] = cred.token

    if not dev_port:
        event.listens_for(engine, "do_connect")(before_connect)

    return engine


def validate_db(engine: Engine, config: AppConfig) -> None:
    """Validate that the database connection works."""
    dev_port = _get_dev_db_port()

    if dev_port:
        logger.info(f"Validating local dev database connection at localhost:{dev_port}")
    else:
        logger.info(
            f"Validating database connection to instance {config.db.instance_name}"
        )
        # check if the database instance exists
        try:
            ws = WorkspaceClient()
            ws.database.get_database_instance(config.db.instance_name)
        except NotFound:
            raise ValueError(
                f"Database instance {config.db.instance_name} does not exist"
            )

    # check if a connection to the database can be established
    try:
        with Session(engine) as session:
            session.connection().execute(text("SELECT 1"))
            session.close()
    except Exception:
        raise ConnectionError("Failed to connect to the database")

    if dev_port:
        logger.info("Local dev database connection validated successfully")
    else:
        logger.info(
            f"Database connection to instance {config.db.instance_name} validated successfully"
        )


def initialize_models(engine: Engine) -> None:
    """Create all SQLModel tables."""
    logger.info("Initializing database models")
    SQLModel.metadata.create_all(engine)
    logger.info("Database models initialized successfully")


# --- Bootstrap ---


def bootstrap_app(app: FastAPI) -> None:
    """
    Bootstrap the FastAPI application by wrapping its lifespan to initialize
    config and WorkspaceClient into app.state.

    If the app already has a lifespan, it is wrapped so that config and
    workspace_client are available in app.state before the original lifespan runs.
    """
    existing_lifespan = app.router.lifespan_context

    @asynccontextmanager
    async def wrapped_lifespan(app: FastAPI):
        config = AppConfig()
        logger.info(f"Starting app with configuration:\n{config}")
        ws = WorkspaceClient()

        app.state.config = config
        app.state.workspace_client = ws

        if existing_lifespan:
            async with existing_lifespan(app):
                yield
        else:
            yield

    app.router.lifespan_context = wrapped_lifespan


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


def get_session(request: Request) -> Generator[Session, None, None]:
    """
    Returns a SQLModel session from the engine stored in app.state.
    """
    if not hasattr(request.app.state, "engine"):
        raise RuntimeError(
            "Database engine not initialized. "
            "Ensure app.state.engine is set during application lifespan startup."
        )
    with Session(request.app.state.engine) as session:
        yield session


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

    Session: TypeAlias = Annotated[Session, Depends(get_session)]
    """SQLModel database session, scoped to the current request.
    Recommended usage: `session: Dependency.Session`"""
