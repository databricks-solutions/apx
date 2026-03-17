"""ASGI profiling middleware for APX vs Uvicorn comparison.

Measures per-request Python-level timing breakdown, identical for both servers.
Activated by env var APX_BENCH_PROFILE=1.

Writes JSONL to /tmp/bench_profile.jsonl inside the container.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from pathlib import Path

PROFILE_PATH = Path("/tmp/bench_profile.jsonl")
_ENABLED = os.environ.get("APX_BENCH_PROFILE", "").lower() in ("1", "true", "yes")
_file = None
_loop_type: str | None = None
_logged_info = False


def _get_file():
    global _file
    if _file is None:
        _file = open(PROFILE_PATH, "a", buffering=8192)
    return _file


def flush() -> None:
    """Flush buffered profiling data to disk."""
    if _file is not None:
        _file.flush()


def _detect_loop_type() -> str:
    loop = asyncio.get_running_loop()
    cls = type(loop)
    module = cls.__module__ or ""
    name = cls.__qualname__
    if "uvloop" in module:
        return "uvloop"
    if "Proactor" in name:
        return "proactor"
    return f"{module}.{name}"


class ProfilingASGIMiddleware:
    """Pure ASGI middleware — zero-copy, no response buffering.

    Measures:
    - total_ns: wall time of `await app(scope, receive, send)`
    - recv_ns / recv_n: cumulative receive() timing and count
    - send_ns / send_n: cumulative send() timing and count
    - handler_ns: total - recv - send (pure framework + handler time)

    Compatible with app.add_middleware() so the FastAPI instance stays
    discoverable by APX's app loader.
    """

    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        global _loop_type, _logged_info
        if _loop_type is None:
            _loop_type = _detect_loop_type()

        if not _logged_info:
            _logged_info = True
            info = {
                "type": "info",
                "loop": _loop_type,
                "python": sys.version.split()[0],
                "pid": os.getpid(),
            }
            f = _get_file()
            f.write(json.dumps(info) + "\n")
            f.flush()

        recv_ns = 0
        recv_count = 0
        send_ns = 0
        send_count = 0

        async def timed_receive():
            nonlocal recv_ns, recv_count
            t0 = time.monotonic_ns()
            msg = await receive()
            recv_ns += time.monotonic_ns() - t0
            recv_count += 1
            return msg

        async def timed_send(message):
            nonlocal send_ns, send_count
            t0 = time.monotonic_ns()
            await send(message)
            send_ns += time.monotonic_ns() - t0
            send_count += 1

        path = scope.get("path", "?")

        t_start = time.monotonic_ns()
        await self.app(scope, timed_receive, timed_send)
        total_ns = time.monotonic_ns() - t_start

        handler_ns = total_ns - recv_ns - send_ns

        record = {
            "type": "req",
            "path": path,
            "total_ns": total_ns,
            "handler_ns": handler_ns,
            "recv_ns": recv_ns,
            "recv_n": recv_count,
            "send_ns": send_ns,
            "send_n": send_count,
        }
        f = _get_file()
        f.write(json.dumps(record) + "\n")


def reset() -> None:
    """Close and delete the profiling file."""
    global _file, _logged_info
    if _file is not None:
        _file.close()
        _file = None
    PROFILE_PATH.unlink(missing_ok=True)
    _logged_info = False


def install_profiling(app) -> None:
    """Add profiling middleware to a FastAPI app if enabled."""
    if _ENABLED:
        app.add_middleware(ProfilingASGIMiddleware)
