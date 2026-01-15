"""Centralized logging for `apx dev` (buffering, routing, and CLI formatting)."""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import io
import logging
import sys
import time
from collections import deque
from collections.abc import Generator
from enum import Enum
from typing import Any, ClassVar, Literal, TypeAlias

from pydantic import BaseModel, ConfigDict
from rich.text import Text
from typing_extensions import override

from apx.models import LogChannel, LogEntry
from apx.utils import console

LogBuffer: TypeAlias = deque[LogEntry]


class DevLogComponent(str, Enum):
    """Where a log originated (used for fine-grained filtering).

    Simplified to 6 essential components:
    - SERVER: APX dev server internal operations
    - BACKEND: User's backend application output
    - UI: Frontend process output
    - BROWSER: Browser console logs (errors/warnings)
    - OPENAPI: OpenAPI schema watcher
    - PROXY: Reverse proxy requests (special: visible by default in [apx] channel)
    """

    SERVER = "server"
    BACKEND = "backend"
    UI = "ui"
    BROWSER = "browser"
    OPENAPI = "openapi"
    PROXY = "proxy"


# Map components to their default log channels
_COMPONENT_DEFAULT_CHANNEL: dict[DevLogComponent, LogChannel] = {
    DevLogComponent.SERVER: LogChannel.APX,
    DevLogComponent.OPENAPI: LogChannel.APX,
    DevLogComponent.PROXY: LogChannel.APX,
    DevLogComponent.BACKEND: LogChannel.APP,
    DevLogComponent.UI: LogChannel.UI,
    DevLogComponent.BROWSER: LogChannel.UI,
}


class _DevLogState(BaseModel):
    model_config: ClassVar[ConfigDict] = ConfigDict(arbitrary_types_allowed=True)

    buffer: LogBuffer | None = None
    configured: bool = False


_STATE = _DevLogState()


# Context variable for routing logs to the correct channel in async contexts
_CURRENT_CHANNEL: contextvars.ContextVar[LogChannel] = contextvars.ContextVar(
    "apx_dev_log_channel", default=LogChannel.APX
)


@contextlib.contextmanager
def log_channel(channel: LogChannel) -> Generator[None, None, None]:
    """Context manager to set the log channel for the current context.

    Used to route uvicorn/framework logs to the correct channel when running
    the backend server.
    """
    token = _CURRENT_CHANNEL.set(channel)
    try:
        yield
    finally:
        _CURRENT_CHANNEL.reset(token)


def set_log_channel(channel: LogChannel) -> contextvars.Token[LogChannel]:
    """Set the current log channel and return a token to reset it.

    Use this for per-request context setting in middleware.
    Call reset_log_channel(token) to restore the previous value.
    """
    return _CURRENT_CHANNEL.set(channel)


def reset_log_channel(token: contextvars.Token[LogChannel]) -> None:
    """Reset the log channel to its previous value using the token."""
    _CURRENT_CHANNEL.reset(token)


def _now_timestamp(created: float | None = None) -> str:
    t = time.localtime(created if created is not None else time.time())
    return time.strftime("%Y-%m-%d %H:%M:%S", t)


def append_log_entry(
    *,
    channel: LogChannel,
    component: DevLogComponent,
    level: str,
    content: str,
    created: float | None = None,
) -> None:
    """Append a log entry to the shared buffer.

    This is the primary way to add logs from anywhere in the dev server.
    """
    if _STATE.buffer is None:
        return
    _STATE.buffer.append(
        LogEntry(
            timestamp=_now_timestamp(created),
            level=level,
            channel=channel,
            component=component.value,
            content=content,
        )
    )


# Keep internal alias for backward compatibility within this module
_append_entry = append_log_entry


class _BufferedLogHandler(logging.Handler):
    """Logging handler that writes to the shared in-memory buffer."""

    buffer_component: DevLogComponent
    buffer_channel: LogChannel

    def __init__(self, *, channel: LogChannel, component: DevLogComponent):
        super().__init__()
        self.buffer_channel = channel
        self.buffer_component = component

    @override
    def emit(self, record: logging.LogRecord) -> None:
        try:
            _append_entry(
                channel=self.buffer_channel,
                component=self.buffer_component,
                level=record.levelname,
                content=self.format(record),
                created=record.created,
            )
        except Exception:
            self.handleError(record)


class _DevServerAccessLogFilter(logging.Filter):
    """Filter noisy access logs for dev-server internal endpoints."""

    _internal_paths: tuple[str, ...] = (
        "/__apx__/logs",
        "/__apx__/status",
        "/__apx__/ports",
        "/__apx__/actions/start",
        "/__apx__/actions/stop",
        "/__apx__/actions/restart",
        "/__apx__/openapi-status",
        "/__apx__/browser-logs",
    )

    @override
    def filter(self, record: logging.LogRecord) -> bool:
        msg = record.getMessage()
        return not any(p in msg for p in self._internal_paths)


def configure_dev_logging(*, buffer: LogBuffer) -> None:
    """Configure all dev loggers to write into the shared in-memory buffer.

    This sets up:
    1. Component loggers (apx.dev.<component>) for our code
    2. Uvicorn access log filter to suppress internal endpoint noise
    """
    _STATE.buffer = buffer

    # Configure component loggers
    for component in DevLogComponent:
        channel = _COMPONENT_DEFAULT_CHANNEL.get(component, LogChannel.APX)
        logger = logging.getLogger(f"apx.dev.{component.value}")
        logger.setLevel(logging.INFO)
        logger.handlers.clear()
        handler = _BufferedLogHandler(channel=channel, component=component)
        handler.setFormatter(logging.Formatter("%(message)s"))
        logger.addHandler(handler)
        logger.propagate = False

    # Configure uvicorn access logger with filter for internal endpoints
    # Route to SERVER component (APX channel) since dev server uvicorn logs are internal
    uvicorn_access = logging.getLogger("uvicorn.access")
    uvicorn_access.setLevel(logging.INFO)
    uvicorn_access.handlers.clear()
    uvicorn_access.addFilter(_DevServerAccessLogFilter())
    uvicorn_handler = _BufferedLogHandler(
        channel=LogChannel.APX, component=DevLogComponent.SERVER
    )
    uvicorn_handler.setFormatter(logging.Formatter("%(message)s"))
    uvicorn_access.addHandler(uvicorn_handler)
    uvicorn_access.propagate = False

    # Suppress uvicorn.error to avoid duplicate startup messages
    uvicorn_error = logging.getLogger("uvicorn.error")
    uvicorn_error.setLevel(logging.WARNING)
    uvicorn_error.handlers.clear()
    uvicorn_error.addHandler(logging.NullHandler())
    uvicorn_error.propagate = False

    _STATE.configured = True


def get_logger(component: DevLogComponent) -> logging.Logger:
    """Get a dev logger for a component.

    Always use this instead of calling logging.getLogger() directly
    to ensure logs are routed to the shared buffer.

    When logging is not configured (e.g., in subprocess contexts like _run_backend),
    logs are written to stderr so they can be captured by collect_subprocess_output.
    """
    logger = logging.getLogger(f"apx.dev.{component.value}")
    if not _STATE.configured:
        # In subprocess contexts (e.g., _run_backend), we need logs to go to stderr
        # so they can be captured by the parent process via collect_subprocess_output.
        if not logger.handlers:
            handler = logging.StreamHandler(sys.stderr)
            handler.setFormatter(logging.Formatter("%(message)s"))
            logger.addHandler(handler)
        logger.setLevel(logging.INFO)
        logger.propagate = False
    return logger


async def collect_subprocess_output(
    process: asyncio.subprocess.Process,
    channel: LogChannel,
    component: DevLogComponent,
) -> None:
    """Collect stdout/stderr from a subprocess and route to log buffer.

    This is the unified way to capture output from frontend and backend
    subprocesses. It reads both streams concurrently and appends each
    line to the shared log buffer.

    Args:
        process: The subprocess to collect output from
        channel: Log channel to route output to (APP, UI, or APX)
        component: Component identifier for the log entry
    """

    async def read_stream(
        stream: asyncio.StreamReader | None,
        level: Literal["INFO", "ERROR"],
    ) -> None:
        if stream is None:
            return
        while True:
            try:
                line = await stream.readline()
                if not line:
                    break
                decoded = line.decode("utf-8", errors="replace").rstrip()
                if decoded:
                    _append_entry(
                        channel=channel,
                        component=component,
                        level=level,
                        content=decoded,
                    )
            except Exception:
                break
            await asyncio.sleep(0.01)

    await asyncio.gather(
        read_stream(process.stdout, "INFO"),
        read_stream(process.stderr, "ERROR"),
    )


@contextlib.contextmanager
def suppress_output_and_logs() -> Generator[None, None, None]:
    """Suppress stdout, stderr and logging output temporarily.

    Used when making SDK calls that may produce unwanted output.
    """
    old_stdout = sys.stdout
    old_stderr = sys.stderr

    root_logger = logging.getLogger()
    original_root_level = root_logger.level
    original_levels: dict[str, int] = {}

    for name in logging.Logger.manager.loggerDict:
        logger = logging.getLogger(name)
        if hasattr(logger, "level"):
            original_levels[name] = logger.level

    try:
        sys.stdout = io.StringIO()
        sys.stderr = io.StringIO()

        root_logger.setLevel(logging.CRITICAL)
        for name in original_levels:
            logging.getLogger(name).setLevel(logging.CRITICAL)

        yield
    finally:
        sys.stdout = old_stdout
        sys.stderr = old_stderr

        root_logger.setLevel(original_root_level)
        for name, level in original_levels.items():
            logging.getLogger(name).setLevel(level)


def print_log_entry(
    entry: LogEntry | dict[str, Any],
    *,
    raw_output: bool = False,  # pyright: ignore[reportExplicitAny]
) -> None:
    """Print a single log entry with `[apx]`/`[app]`/`[ui]` prefixes."""
    if isinstance(entry, dict):
        entry = LogEntry.model_validate(entry)

    if raw_output:
        print(entry.content)
        return

    prefix_style = (
        "bright_blue"
        if entry.channel == LogChannel.APX
        else "yellow"
        if entry.channel == LogChannel.APP
        else "cyan"
    )

    ts = Text(entry.timestamp, style="dim")
    sep = Text(" | ")
    prefix = Text(f"[{entry.channel.value}]", style=prefix_style)
    content = Text(entry.content)
    console.print(ts + sep + prefix + sep + content)
