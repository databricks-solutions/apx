from collections.abc import Generator
from pathlib import Path
from typing import Any

__version__: str

def run_cli(args: list[str]) -> int: ...
def generate_openapi(project_root: Path) -> bool: ...
def get_dotenv_vars() -> dict[str, str]: ...

# ── Exceptions ───────────────────────────────────────────────────────────

class NotFound(Exception):
    """Return a 404 Not Found response."""
    ...

class BadRequest(Exception):
    """Return a 400 Bad Request response."""
    ...

class Forbidden(Exception):
    """Return a 403 Forbidden response."""
    ...

# ── Scheduler primitives ─────────────────────────────────────────────────

class Event:
    def is_set(self) -> bool: ...
    def set(self) -> None: ...
    def wait(self) -> EventWaiter: ...

class EventWaiter:
    def __await__(self) -> Generator[Any, None, None]: ...

class Future:
    @property
    def done(self) -> bool: ...
    def __await__(self) -> Generator[Any, None, Any]: ...

# ── Telemetry ────────────────────────────────────────────────────────────

class SpanHandle:
    """OTEL span usable as sync/async context manager."""
    def __init__(
        self, name: str, attributes: dict[str, str] | None = None
    ) -> None: ...
    def __enter__(self) -> SpanHandle: ...
    def __exit__(
        self,
        _exc_type: type[BaseException] | None = None,
        _exc_val: BaseException | None = None,
        _exc_tb: object | None = None,
    ) -> bool: ...
    async def __aenter__(self) -> SpanHandle: ...
    async def __aexit__(
        self,
        _exc_type: type[BaseException] | None = None,
        _exc_val: BaseException | None = None,
        _exc_tb: object | None = None,
    ) -> bool: ...
    def add_event(
        self, name: str, attributes: dict[str, str] | None = None
    ) -> None: ...
    def set_attribute(self, key: str, value: str) -> None: ...

class RustCounter:
    """OTLP counter backed by Rust."""
    def inc(self, value: int = 1, labels: dict[str, str] | None = None) -> None: ...

class RustHistogram:
    """OTLP histogram backed by Rust."""
    def observe(self, value: float, labels: dict[str, str] | None = None) -> None: ...

class RustGauge:
    """OTLP gauge backed by Rust."""
    def set(self, value: float, labels: dict[str, str] | None = None) -> None: ...

def create_counter(
    name: str, description: str = "", unit: str = ""
) -> RustCounter: ...
def create_histogram(
    name: str, description: str = "", unit: str = ""
) -> RustHistogram: ...
def create_gauge(
    name: str, description: str = "", unit: str = ""
) -> RustGauge: ...
def _emit_log(level: int, message: str, logger_name: str) -> None: ...
