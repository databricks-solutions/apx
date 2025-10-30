"""Logging utilities for APX dev server."""

import contextlib
import io
import logging
import sys
import time
from collections import deque
from typing import Any, TypeAlias

from typing_extensions import override

from apx.cli.dev.models import LogEntry
from apx.utils import console, PrefixedLogHandler


LogBuffer: TypeAlias = deque[LogEntry]


# === Log Handlers ===


class BufferedLogHandler(logging.Handler):
    """Custom log handler that stores logs in memory buffer."""

    buffer: LogBuffer
    process_name: str

    def __init__(self, buffer: LogBuffer, process_name: str):
        super().__init__()
        self.buffer = buffer
        self.process_name = process_name

    @override
    def emit(self, record: logging.LogRecord) -> None:
        """Emit a log record to the buffer."""
        try:
            log_entry = LogEntry(
                timestamp=time.strftime(
                    "%Y-%m-%d %H:%M:%S", time.localtime(record.created)
                ),
                level=record.levelname,
                process_name=self.process_name,
                content=self.format(record),
            )
            self.buffer.append(log_entry)
        except Exception:
            self.handleError(record)


class LoggerWriter:
    """Logger writer for redirecting stdout/stderr to the backend logger."""

    def __init__(self, logger: logging.Logger, level: int, prefix: str):
        self.logger: logging.Logger = logger
        self.level: int = level
        self.prefix: str = prefix
        self.buffer: str = ""

    def write(self, message: str | None) -> None:
        if not message:
            return
        self.buffer += message
        lines = self.buffer.split("\n")
        self.buffer = lines[-1]
        for line in lines[:-1]:
            if line:
                self.logger.log(self.level, f"{self.prefix} | {line}")

    def flush(self):
        if self.buffer:
            self.logger.log(self.level, f"{self.prefix} | {self.buffer}")
            self.buffer = ""

    def isatty(self):
        return False


class DevServerAccessLogFilter(logging.Filter):
    """Filter to exclude dev server internal API logs from access logs."""

    @override
    def filter(self, record: logging.LogRecord) -> bool:
        """Return False for dev server internal endpoints to exclude them from logs."""
        message = record.getMessage()
        # Exclude logs for dev server internal endpoints
        internal_paths = ["/logs", "/status", "/start", "/stop", "/restart", "/ports"]
        for path in internal_paths:
            if f'"{path}' in message or f"'{path}" in message:
                return False
        return True


# === Setup Functions ===


def setup_buffered_logging(buffer: LogBuffer, process_name: str):
    """Setup logging that writes to in-memory buffer only.

    Args:
        buffer: The log buffer to write to
        process_name: Name of the process (frontend, backend, openapi)
    """
    logger = logging.getLogger(f"apx.{process_name}")
    logger.setLevel(logging.INFO)
    logger.handlers.clear()

    # Buffer handler (in-memory only)
    buffer_handler = BufferedLogHandler(buffer, process_name)
    buffer_formatter = logging.Formatter("%(message)s")
    buffer_handler.setFormatter(buffer_formatter)
    logger.addHandler(buffer_handler)

    logger.propagate = False


def setup_uvicorn_logging(use_memory: bool = False):
    """Configure uvicorn loggers to use in-memory buffer or console.

    Args:
        use_memory: If True, use the in-memory logger (set up by dev_server)
                    If False, use console logging (for standalone mode)
    """
    # Configure ONLY uvicorn loggers
    for logger_name in ["uvicorn", "uvicorn.access", "uvicorn.error"]:
        logger = logging.getLogger(logger_name)
        logger.handlers.clear()

        # Add filter to uvicorn.access to exclude dev server internal API logs
        if logger_name == "uvicorn.access":
            logger.addFilter(DevServerAccessLogFilter())

        if use_memory:
            # Use the backend logger that's already configured by dev_server
            backend_logger = logging.getLogger("apx.backend")
            if backend_logger.handlers:
                # Create a wrapper handler that adds "BE | " prefix
                class PrefixedHandler(logging.Handler):
                    def __init__(self, base_handler: logging.Handler, prefix: str):
                        super().__init__()
                        self.base_handler: logging.Handler = base_handler
                        self.prefix: str = prefix

                    @override
                    def emit(self, record: logging.LogRecord) -> None:
                        # Add prefix to the message
                        original_msg = record.getMessage()
                        record.msg = f"{self.prefix} | {original_msg}"
                        record.args = ()
                        self.base_handler.emit(record)

                # Add the prefixed handler
                for base_handler in backend_logger.handlers:
                    prefixed_handler = PrefixedHandler(base_handler, "BE")
                    logger.addHandler(prefixed_handler)
            else:
                # Fallback to console if no handlers found
                handler: logging.Handler = PrefixedLogHandler(
                    prefix="[backend]", color="aquamarine1"
                )
                formatter = logging.Formatter("%(message)s")
                handler.setFormatter(formatter)
                logger.addHandler(handler)
        else:
            # Console logging (no buffering needed)
            handler = PrefixedLogHandler(prefix="[backend]", color="aquamarine1")
            formatter = logging.Formatter("%(message)s")
            handler.setFormatter(formatter)
            logger.addHandler(handler)

        logger.setLevel(logging.INFO)
        logger.propagate = False


# === Utility Functions ===


@contextlib.contextmanager
def suppress_output_and_logs():
    """Suppress stdout, stderr and logging output temporarily."""
    # Save original stdout/stderr
    old_stdout = sys.stdout
    old_stderr = sys.stderr

    # Save original log level for root logger and all existing loggers
    root_logger = logging.getLogger()
    original_root_level = root_logger.level
    original_levels = {}

    for name in logging.Logger.manager.loggerDict:
        logger = logging.getLogger(name)
        if hasattr(logger, "level"):
            original_levels[name] = logger.level

    try:
        # Redirect stdout/stderr to devnull
        sys.stdout = io.StringIO()
        sys.stderr = io.StringIO()

        # Set all loggers to CRITICAL to suppress INFO/WARNING logs
        root_logger.setLevel(logging.CRITICAL)
        for name in original_levels:
            logging.getLogger(name).setLevel(logging.CRITICAL)

        yield
    finally:
        # Restore stdout/stderr
        sys.stdout = old_stdout
        sys.stderr = old_stderr

        # Restore log levels
        root_logger.setLevel(original_root_level)
        for name, level in original_levels.items():
            logging.getLogger(name).setLevel(level)


def print_log_entry(log: dict[str, Any], raw_output: bool = False):
    """Print a single log entry with formatting.

    Args:
        log: Log entry dict with keys: process_name, content, timestamp
        raw_output: If True, print raw log content without prefix formatting
    """
    content = log.get("content", "")
    timestamp = log.get("timestamp", log.get("created_at", ""))

    # For raw output, strip the internal prefix and print without color/formatting
    if raw_output:
        # For backend logs, strip the "BE | " or "APP | " prefix
        if log["process_name"] == "backend" and " | " in content:
            stream_prefix, message = content.split(" | ", 1)
            if stream_prefix in ["BE", "APP"]:
                content = message
        # Print without rich formatting
        print(f"{content}")
        return

    # For backend logs, parse the stream prefix (BE or APP)
    if log["process_name"] == "backend":
        # Content format is "BE | message" or "APP | message"
        if " | " in content:
            stream_prefix, message = content.split(" | ", 1)
            if stream_prefix == "BE":
                prefix_color = "green"
                prefix = "[BE] "
                content = message
            elif stream_prefix == "APP":
                prefix_color = "yellow"
                prefix = "[APP]"
                content = message
            else:
                # Fallback if unknown stream
                prefix_color = "green"
                prefix = "[BE] "
        else:
            # No stream prefix found, default to BE
            prefix_color = "green"
            prefix = "[BE] "
    elif log["process_name"] == "frontend":
        prefix_color = "cyan"
        prefix = "[UI] "
    elif log["process_name"] == "openapi":
        # OpenAPI watcher logs come directly from the logger (no stream prefix)
        prefix_color = "magenta"
        prefix = "[GEN]"
    else:
        # Default fallback
        prefix_color = "white"
        prefix = f"[{log['process_name'].upper()}]".ljust(5)

    console.print(
        f"[dim]{timestamp}[/dim] | [{prefix_color}]{prefix}[/{prefix_color}] | {content}"
    )
