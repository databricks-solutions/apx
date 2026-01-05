"""Centralized logging for `apx dev` (buffering, routing, and CLI formatting)."""

from __future__ import annotations

import contextlib
import contextvars
import io
import logging
import sys
import time
from collections import deque
from enum import Enum
from typing import Any, ClassVar, Literal, TypeAlias
from collections.abc import Generator

from pydantic import BaseModel, ConfigDict
from rich.text import Text
from typing_extensions import override

from apx.models import LogChannel, LogEntry
from apx.utils import console

LogBuffer: TypeAlias = deque[LogEntry]


class DevLogComponent(str, Enum):
    """Where a log originated (used for fine-grained filtering)."""

    SERVER = "server"
    SERVER_UVICORN = "server_uvicorn"
    BACKEND = "backend"
    BACKEND_UVICORN = "backend_uvicorn"
    APP = "app"
    UI = "ui"
    BROWSER = "browser"
    OPENAPI = "openapi"
    PROXY = "proxy"
    PROCESS_CONTROL = "process_control"
    RETRY = "retry"


_COMPONENT_DEFAULT_CHANNEL: dict[DevLogComponent, LogChannel] = {
    DevLogComponent.SERVER: LogChannel.APX,
    DevLogComponent.SERVER_UVICORN: LogChannel.APX,
    DevLogComponent.OPENAPI: LogChannel.APX,
    DevLogComponent.PROXY: LogChannel.APX,
    DevLogComponent.PROCESS_CONTROL: LogChannel.APX,
    DevLogComponent.RETRY: LogChannel.APX,
    DevLogComponent.BACKEND: LogChannel.APP,
    DevLogComponent.BACKEND_UVICORN: LogChannel.APP,
    DevLogComponent.APP: LogChannel.APP,
    DevLogComponent.UI: LogChannel.UI,
    DevLogComponent.BROWSER: LogChannel.UI,
}


_CURRENT_CHANNEL: contextvars.ContextVar[LogChannel] = contextvars.ContextVar(
    "apx_dev_log_channel", default=LogChannel.APX
)


@contextlib.contextmanager
def log_channel(channel: LogChannel) -> Generator[None, None, None]:
    """Context manager to route uvicorn/stdout logs to a specific channel."""
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


class _DevLogState(BaseModel):
    model_config: ClassVar[ConfigDict] = ConfigDict(arbitrary_types_allowed=True)

    buffer: LogBuffer | None = None
    configured: bool = False


_STATE = _DevLogState()


def _now_timestamp(created: float | None = None) -> str:
    t = time.localtime(created if created is not None else time.time())
    return time.strftime("%Y-%m-%d %H:%M:%S", t)


def _append_entry(
    *,
    channel: LogChannel,
    component: DevLogComponent,
    level: str,
    content: str,
    created: float | None = None,
) -> None:
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


class _BufferedLogHandler(logging.Handler):
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
    )

    @override
    def filter(self, record: logging.LogRecord) -> bool:
        msg = record.getMessage()
        return not any(p in msg for p in self._internal_paths)


class _UvicornRoutingHandler(logging.Handler):
    """Route uvicorn loggers to [apx] vs [app] based on the active context."""

    @override
    def emit(self, record: logging.LogRecord) -> None:
        try:
            channel = _CURRENT_CHANNEL.get()

            # Filter out DEBUG logs from dev server (APX channel) to reduce noise.
            # Only user app (APP channel) gets DEBUG-level uvicorn logs.
            if record.levelno <= logging.DEBUG and channel != LogChannel.APP:
                return

            component = (
                DevLogComponent.BACKEND_UVICORN
                if channel == LogChannel.APP
                else DevLogComponent.SERVER_UVICORN
            )
            _append_entry(
                channel=channel,
                component=component,
                level=record.levelname,
                content=self.format(record),
                created=record.created,
            )
        except Exception:
            self.handleError(record)


def configure_dev_logging(*, buffer: LogBuffer) -> None:
    """Configure all dev loggers to write into the shared in-memory buffer."""
    _STATE.buffer = buffer

    # Component loggers used by our code (everything else should call get_logger()).
    for component in DevLogComponent:
        channel = _COMPONENT_DEFAULT_CHANNEL.get(component, LogChannel.APX)
        logger = logging.getLogger(f"apx.dev.{component.value}")
        logger.setLevel(logging.INFO)
        logger.handlers.clear()
        handler = _BufferedLogHandler(channel=channel, component=component)
        handler.setFormatter(logging.Formatter("%(message)s"))
        logger.addHandler(handler)
        logger.propagate = False

    # Uvicorn loggers are shared between the dev server and the user backend.
    # Route them via a contextvar set by the caller before running uvicorn.
    # Set to DEBUG to capture verbose logs from user app (filtered for dev server).
    for name in ("uvicorn", "uvicorn.error", "uvicorn.access"):
        uv = logging.getLogger(name)
        uv.setLevel(logging.DEBUG)
        uv.handlers.clear()
        if name == "uvicorn.access":
            uv.addFilter(_DevServerAccessLogFilter())
        h = _UvicornRoutingHandler()
        h.setFormatter(logging.Formatter("%(message)s"))
        uv.addHandler(h)
        uv.propagate = False

    # Starlette and FastAPI loggers - route to [app] channel via context.
    # This captures ServerErrorMiddleware exception logging and other framework logs.
    for name in (
        "starlette",
        "starlette.middleware",
        "starlette.middleware.errors",
        "fastapi",
    ):
        framework_logger = logging.getLogger(name)
        framework_logger.setLevel(logging.INFO)
        framework_logger.handlers.clear()
        h = _UvicornRoutingHandler()  # Reuse routing handler for context-aware routing
        h.setFormatter(logging.Formatter("%(message)s"))
        framework_logger.addHandler(h)
        framework_logger.propagate = False

    _STATE.configured = True


def get_logger(component: DevLogComponent) -> logging.Logger:
    """Get a dev logger for a component (do not call stdlib logging directly)."""
    logger = logging.getLogger(f"apx.dev.{component.value}")
    if not _STATE.configured:
        # Avoid \"No handlers could be found\" warnings in contexts that don't configure dev logging.
        if not logger.handlers:
            logger.addHandler(logging.NullHandler())
        logger.propagate = False
    return logger


class ContextualStreamWriter:
    """Context-aware stdout/stderr writer that buffers into dev log channels."""

    def __init__(
        self,
        *,
        level_name: Literal["INFO", "ERROR"],
    ) -> None:
        self._level_name: Literal["INFO", "ERROR"] = level_name
        self._buffer: str = ""

    def write(self, message: str | None) -> None:
        if not message:
            return
        self._buffer += message
        lines = self._buffer.split("\n")
        self._buffer = lines[-1]
        for line in lines[:-1]:
            if not line:
                continue
            channel = _CURRENT_CHANNEL.get()
            component = (
                DevLogComponent.APP
                if channel == LogChannel.APP
                else (
                    DevLogComponent.SERVER
                    if channel == LogChannel.APX
                    else DevLogComponent.UI
                )
            )
            _append_entry(
                channel=channel,
                component=component,
                level=self._level_name,
                content=line,
                created=None,
            )

    def flush(self) -> None:
        if not self._buffer:
            return
        channel = _CURRENT_CHANNEL.get()
        component = (
            DevLogComponent.APP
            if channel == LogChannel.APP
            else (
                DevLogComponent.SERVER
                if channel == LogChannel.APX
                else DevLogComponent.UI
            )
        )
        _append_entry(
            channel=channel,
            component=component,
            level=self._level_name,
            content=self._buffer,
            created=None,
        )
        self._buffer = ""

    def isatty(self) -> bool:
        return False


@contextlib.contextmanager
def suppress_output_and_logs() -> Generator[None, None, None]:
    """Suppress stdout, stderr and logging output temporarily."""
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
