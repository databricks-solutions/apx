"""SQLite engine and session factory for the bencher service."""
from __future__ import annotations

import logging
import tempfile

from sqlmodel import Session, SQLModel, create_engine

logger = logging.getLogger("bencher.database")

_tmpdir = tempfile.mkdtemp(prefix="bencher_")
_db_path = f"{_tmpdir}/bencher.db"
engine = create_engine(
    f"sqlite:///{_db_path}",
    connect_args={"check_same_thread": False},
)


def create_db() -> None:
    """Create all tables."""
    logger.info("Creating database at %s", _db_path)
    SQLModel.metadata.create_all(engine)


def get_session():
    """Yield a SQLModel session (FastAPI dependency)."""
    with Session(engine) as session:
        yield session
