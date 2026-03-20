"""Lakebase Autoscaled (PostgreSQL-compatible) engine and session factory for the bencher service."""

from __future__ import annotations

import logging
import os

from sqlalchemy import event, text
from sqlmodel import Session, SQLModel, create_engine

logger = logging.getLogger("bencher.database")

_engine = None


def init_engine() -> None:
    """Discover Lakebase endpoint and create a SQLAlchemy engine with auto-refreshing credentials."""
    global _engine

    from databricks.sdk import WorkspaceClient

    project_id = os.environ.get("BENCH_PG_PROJECT_ID")
    if not project_id:
        raise RuntimeError("BENCH_PG_PROJECT_ID environment variable is required")

    ws = WorkspaceClient()

    # Discover endpoint: list branches → list endpoints → get endpoint host.
    project_parent = f"projects/{project_id}"
    branches = list(ws.postgres.list_branches(parent=project_parent))
    if not branches:
        raise RuntimeError(f"No branches found for Lakebase project {project_id}")
    branch = branches[0]
    logger.info("Using branch: %s", branch.name)

    endpoints = list(ws.postgres.list_endpoints(parent=branch.name))
    if not endpoints:
        raise RuntimeError(f"No endpoints found for branch {branch.name}")
    endpoint = ws.postgres.get_endpoint(name=endpoints[0].name)
    host = endpoint.status.hosts.host
    logger.info("Connecting to Lakebase: host=%s, port=5432", host)

    # Username: prefer client_id (SP auth), fall back to current user.
    username = ws.config.client_id or ws.current_user.me().user_name

    # Build engine URL — password is injected via do_connect event.
    url = f"postgresql+psycopg://{username}:@{host}:5432/databricks_postgres"
    _engine = create_engine(
        url,
        pool_size=4,
        pool_recycle=45 * 60,
        connect_args={"sslmode": "require"},
    )

    # Store references for credential refresh.
    _engine._lakebase_ws = ws  # type: ignore[attr-defined]
    _engine._lakebase_endpoint = endpoint.name  # type: ignore[attr-defined]

    @event.listens_for(_engine, "do_connect")
    def _refresh_token(dialect, conn_rec, cargs, cparams):
        """Inject a fresh OAuth token before each new physical connection."""
        cred = ws.postgres.generate_database_credential(endpoint=endpoint.name)
        cparams["password"] = cred.token

    logger.info("Engine created (pool_size=4, pool_recycle=45min)")


def get_engine():
    """Return the module-level engine singleton, or raise if not initialized."""
    if _engine is None:
        raise RuntimeError("Database engine not initialized — call init_engine() first")
    return _engine


def create_db() -> None:
    """Create all tables via CREATE TABLE IF NOT EXISTS, then apply migrations."""
    logger.info("Creating tables via SQLModel.metadata.create_all()")
    SQLModel.metadata.create_all(get_engine())
    _migrate(get_engine())


def _migrate(engine) -> None:
    """Apply incremental schema migrations for columns added after initial deploy."""
    migrations = []
    if not migrations:
        return
    with engine.connect() as conn:
        for table, column, col_type in migrations:
            result = conn.execute(
                text(
                    "SELECT 1 FROM information_schema.columns "
                    "WHERE table_name = :table AND column_name = :column"
                ),
                {"table": table, "column": column},
            )
            if result.fetchone() is None:
                logger.info("Migrating: ALTER TABLE %s ADD COLUMN %s", table, column)
                conn.execute(
                    text(f"ALTER TABLE {table} ADD COLUMN {column} {col_type}")
                )
        conn.commit()


def get_session():
    """Yield a SQLModel session (FastAPI dependency)."""
    with Session(get_engine()) as session:
        yield session
