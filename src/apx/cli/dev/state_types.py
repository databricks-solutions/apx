"""Shared typing-only state contracts for the dev server.

These live in a dedicated module to avoid type-only circular imports between
`server.py` (runtime state owner) and `manager.py` (process runner utilities).
"""

from __future__ import annotations

import asyncio
from typing import Protocol

from apx.cli.dev.process_control import TrackedProcess


class FrontendProcessState(Protocol):
    """Minimal state surface needed by `run_frontend_with_logging`."""

    frontend_process: asyncio.subprocess.Process | None
    frontend_tracked: TrackedProcess | None
