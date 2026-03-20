"""FastAPI application for the bencher service."""

from __future__ import annotations

import json
import logging
import stat
import tempfile
import time
from contextlib import asynccontextmanager
from pathlib import Path

import httpx
from fastapi import FastAPI, Request

from .database import create_db, get_engine, init_engine
from .models import Scenario
from .routes import router
from .runner import set_default_scenarios

logger = logging.getLogger("bencher")

OHA_VERSION = "1.14.0"


async def install_oha() -> str:
    """Download oha binary for linux-amd64 from GitHub releases."""
    url = (
        f"https://github.com/hatoo/oha/releases/download/v{OHA_VERSION}/oha-linux-amd64"
    )
    dest = Path(tempfile.mkdtemp(prefix="oha_")) / "oha"

    logger.info("Downloading oha v%s from %s", OHA_VERSION, url)
    async with httpx.AsyncClient() as client:
        resp = await client.get(url, follow_redirects=True, timeout=120.0)
        resp.raise_for_status()
        dest.write_bytes(resp.content)

    dest.chmod(dest.stat().st_mode | stat.S_IEXEC)
    logger.info("oha installed at %s (%d bytes)", dest, dest.stat().st_size)
    return str(dest)


def _load_default_scenarios() -> list[Scenario]:
    """Load scenarios.json from the package directory (copied during assembly)."""
    for candidate in [
        Path(__file__).resolve().parent / "scenarios.json",
        Path(__file__).resolve().parent.parent / "scenarios.json",
    ]:
        if candidate.exists():
            logger.info("Loading scenarios from %s", candidate)
            raw = json.loads(candidate.read_text())
            return [Scenario(**s) for s in raw]
    logger.warning("No scenarios.json found")
    return []


@asynccontextmanager
async def lifespan(app: FastAPI):
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s [%(name)s] %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )
    logger.info("Bencher starting up...")

    # 1. Install oha binary.
    oha_path = await install_oha()
    app.state.oha_path = oha_path

    # 2. Initialize database.
    init_engine()
    create_db()
    logger.info("Database initialized")

    # 3. Load default scenarios.
    scenarios = _load_default_scenarios()
    set_default_scenarios(scenarios)
    logger.info("Loaded %d default scenarios", len(scenarios))

    logger.info("Bencher ready")
    yield
    get_engine().dispose()
    logger.info("Bencher shutting down")


app = FastAPI(title="APX Bencher", lifespan=lifespan)


@app.middleware("http")
async def log_requests(request: Request, call_next):
    t0 = time.monotonic()
    response = await call_next(request)
    elapsed_ms = (time.monotonic() - t0) * 1000
    logger.info(
        "%s %s → %d (%.1fms)",
        request.method,
        request.url.path,
        response.status_code,
        elapsed_ms,
    )
    return response


app.include_router(router)
