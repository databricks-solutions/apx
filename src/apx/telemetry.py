"""APX Telemetry — spans, metrics, and structured logs via OTLP.

Usage::

    from apx.telemetry import span, log, Counter, Histogram, Gauge, Unit

    # Spans (context manager / decorator)
    with span("db.query", table="users") as s:
        s.set_attribute("rows", "42")

    async with span("fetch_data"):
        await db.fetch()

    @span("load_user")
    async def load_user(uid: int): ...

    # Structured logs (instant spans with a level attribute)
    log.info("request handled", method="GET", status=200)
    log.warn("slow query", duration_ms=1200)

    # Metrics
    counter = Counter("http.requests", description="Total requests", unit=Unit.requests)
    counter.inc()

    histogram = Histogram("http.latency", unit=Unit.milliseconds)
    histogram.observe(42.0)
"""

from __future__ import annotations

import asyncio
import functools
import os
import sys
import traceback
from typing import Annotated, Any, Callable, ClassVar, Literal, TypeVar, Union

from pydantic import BaseModel, Discriminator, Field, Tag

from apx._core import (
    RustCounter,
    RustGauge,
    RustHistogram,
    SpanHandle,
    StatusCode,
    create_counter as _create_counter,
    create_gauge as _create_gauge,
    create_histogram as _create_histogram,
)

_F = TypeVar("_F", bound=Callable[..., Any])


# ── Worker identity ──────────────────────────────────────────────────────
# Resolved once at import time from env vars set by the supervisor.
# These attributes are merged into every span and log-level span.


def _resolve_identity() -> dict[str, str]:
    worker_id = os.environ.get("APX_WORKER_ID")
    if worker_id is not None:
        return {"apx.role": "worker", "apx.worker.id": worker_id}
    if os.environ.get("APX_WORKER_NONCE") is not None:
        return {"apx.role": "worker"}
    return {"apx.role": "supervisor"}


_IDENTITY_ATTRS: dict[str, str] = _resolve_identity()

__all__ = [
    "span",
    "log",
    "StatusCode",
    "Unit",
    "Counter",
    "Histogram",
    "Gauge",
    "configure",
    "Configuration",
    "HttpInstrumentation",
    "HttpMetrics",
    "SystemInstrumentation",
    "SystemMetrics",
    "ProcessInstrumentation",
    "ProcessMetrics",
    "ApxInstrumentation",
    "ApxMetrics",
    "CaptureHeaders",
    "Instrumentation",
]


# ── Unit ─────────────────────────────────────────────────────────────────


class Unit(str):
    """Metric unit following UCUM notation.

    Use predefined constants (``Unit.seconds``, ``Unit.milliseconds``, ...)
    or pass any custom string::

        Counter("widgets.produced", unit=Unit.requests)
        Counter("custom_thing", unit="widgets")
    """

    seconds: ClassVar[Unit]
    milliseconds: ClassVar[Unit]
    bytes: ClassVar[Unit]
    kilobytes: ClassVar[Unit]
    megabytes: ClassVar[Unit]
    requests: ClassVar[Unit]
    ratio: ClassVar[Unit]
    percent: ClassVar[Unit]
    dimensionless: ClassVar[Unit]


Unit.seconds = Unit("s")
Unit.milliseconds = Unit("ms")
Unit.bytes = Unit("By")
Unit.kilobytes = Unit("kBy")
Unit.megabytes = Unit("MBy")
Unit.requests = Unit("1")
Unit.ratio = Unit("1")
Unit.percent = Unit("%")
Unit.dimensionless = Unit("1")


# ── span ─────────────────────────────────────────────────────────────────


class span:
    """Context manager / decorator for creating trace spans.

    Keyword arguments become span attributes (values are stringified).
    """

    def __init__(self, name: str, **attributes: Any) -> None:
        self._name = name
        self._attributes = {k: str(v) for k, v in attributes.items()}
        self._handle: SpanHandle | None = None

    def _merged_attrs(self) -> dict[str, str]:
        return {**_IDENTITY_ATTRS, **self._attributes}

    def __enter__(self) -> SpanHandle:
        self._handle = SpanHandle(self._name, self._merged_attrs())
        self._handle.__enter__()
        return self._handle

    def __exit__(
        self,
        exc_type: type[BaseException] | None = None,
        exc_val: BaseException | None = None,
        exc_tb: object | None = None,
    ) -> bool:
        if self._handle is not None:
            return self._handle.__exit__(exc_type, exc_val, exc_tb)
        return False

    async def __aenter__(self) -> SpanHandle:
        self._handle = SpanHandle(self._name, self._merged_attrs())
        self._handle.__enter__()
        return self._handle

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None = None,
        exc_val: BaseException | None = None,
        exc_tb: object | None = None,
    ) -> bool:
        if self._handle is not None:
            return self._handle.__exit__(exc_type, exc_val, exc_tb)
        return False

    def __call__(self, fn: _F) -> _F:
        if asyncio.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                async with span(self._name, **self._attributes):
                    return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            with span(self._name, **self._attributes):
                return fn(*args, **kwargs)

        return sync_wrapper  # type: ignore[return-value]


# ── log ──────────────────────────────────────────────────────────────────


def _emit_log_span(level: str, message: str, **attributes: Any) -> None:
    """Create an instant (zero-duration) span representing a log event."""
    attrs = {**_IDENTITY_ATTRS, **{k: str(v) for k, v in attributes.items()}}
    attrs["log.level"] = level
    handle = SpanHandle(message, attrs)
    handle.__enter__()
    handle.__exit__(None, None, None)


class _Log:
    """Namespace for structured log-level functions.

    Each method creates an instant span with a ``log.level`` attribute,
    matching the Logfire model where logs are zero-duration spans.

    Usage::

        from apx.telemetry import log

        log.info("request handled", method="GET", status=200)
        log.warn("slow query", duration_ms=1200)
    """

    __slots__ = ()

    @staticmethod
    def trace(message: str, **attributes: Any) -> None:
        """Emit a TRACE-level log span."""
        _emit_log_span("trace", message, **attributes)

    @staticmethod
    def debug(message: str, **attributes: Any) -> None:
        """Emit a DEBUG-level log span."""
        _emit_log_span("debug", message, **attributes)

    @staticmethod
    def info(message: str, **attributes: Any) -> None:
        """Emit an INFO-level log span."""
        _emit_log_span("info", message, **attributes)

    @staticmethod
    def notice(message: str, **attributes: Any) -> None:
        """Emit a NOTICE-level log span."""
        _emit_log_span("notice", message, **attributes)

    @staticmethod
    def warn(message: str, **attributes: Any) -> None:
        """Emit a WARN-level log span."""
        _emit_log_span("warn", message, **attributes)

    @staticmethod
    def error(message: str, **attributes: Any) -> None:
        """Emit an ERROR-level log span."""
        _emit_log_span("error", message, **attributes)

    @staticmethod
    def fatal(message: str, **attributes: Any) -> None:
        """Emit a FATAL-level log span."""
        _emit_log_span("fatal", message, **attributes)

    @staticmethod
    def exception(message: str, **attributes: Any) -> None:
        """Emit an ERROR-level log span with the current exception attached.

        Must be called from an ``except`` block.
        """
        exc_info = sys.exc_info()
        attrs = {**_IDENTITY_ATTRS, **{k: str(v) for k, v in attributes.items()}}
        attrs["log.level"] = "error"
        if exc_info[1] is not None:
            attrs["exception.type"] = type(exc_info[1]).__qualname__
            attrs["exception.message"] = str(exc_info[1])
            attrs["exception.stacktrace"] = "".join(
                traceback.format_exception(*exc_info)
            )
        handle = SpanHandle(message, attrs)
        handle.__enter__()
        handle.__exit__(None, None, None)


log = _Log()


# ── Metrics ──────────────────────────────────────────────────────────────


class Counter:
    """OTLP counter metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustCounter = _create_counter(name, description, str(unit))

    def inc(self, value: int = 1, *, labels: dict[str, str] | None = None) -> None:
        """Increment the counter."""
        self._instrument.inc(value, labels)


class Histogram:
    """OTLP histogram metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustHistogram = _create_histogram(
            name, description, str(unit)
        )

    def observe(self, value: float, *, labels: dict[str, str] | None = None) -> None:
        """Record an observation."""
        self._instrument.observe(value, labels)


class Gauge:
    """OTLP gauge metric backed by Rust."""

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustGauge = _create_gauge(name, description, str(unit))

    def set(self, value: float, *, labels: dict[str, str] | None = None) -> None:
        """Set the gauge value."""
        self._instrument.set(value, labels)


# ── Instrumentation configuration ────────────────────────────────────────


class ProcessMetrics(BaseModel):
    """Per-process metric toggles.

    Collected per-worker (and once for the supervisor process itself).
    Each worker reports its own CPU, memory, and thread count under
    the OTEL Resource attribute ``apx.worker.id``; the supervisor
    reports under ``apx.role=supervisor``.

    Metric names, descriptions, and units are defined in the Rust
    metric definitions module (``telemetry/defs.rs``).
    """

    cpu: bool = True
    memory: bool = False
    threads: bool = False


class SystemMetrics(BaseModel):
    """Machine-wide metric toggles.

    Collected once on the supervisor process only. These are global
    system gauges (CPU, memory, swap, disk I/O, network I/O) that
    are identical regardless of which process reads them, so only
    the supervisor collects them to avoid N redundant copies.

    Metric names, descriptions, and units are defined in the Rust
    metric definitions module (``telemetry/defs.rs``).
    """

    cpu: bool = True
    memory: bool = True
    swap: bool = False
    disk_io: bool = False
    network_io: bool = False


class HttpMetrics(BaseModel):
    """HTTP server metric toggles.

    Collected per-worker. Each worker reports its own request duration
    and active request count. The OTEL Resource attribute
    ``apx.worker.id`` distinguishes workers; aggregate across all
    workers at query time (e.g. ``sum(rate(...))``) for server-wide
    totals.
    """

    server_request_duration: bool = True
    server_active_requests: bool = True


class ApxMetrics(BaseModel):
    """APX framework dispatch pipeline metric toggles.

    Collected per-worker. Each histogram records latency for the
    dispatch phases within a single worker process. Use the OTEL
    Resource attribute ``apx.worker.id`` to drill down; aggregate
    across workers for server-wide distributions.

    All metrics default to disabled (opt-in for low overhead).
    """

    dispatch_body_collect: bool = False
    dispatch_crossbeam_send: bool = False
    dispatch_response_wait: bool = False
    dispatch_total: bool = False
    asgi_receive_build: bool = False
    asgi_send_parse: bool = False


class CaptureHeaders(BaseModel):
    """HTTP header capture rules."""

    request: list[str] = Field(default_factory=list)
    response: list[str] = Field(default_factory=list)
    sanitize: list[str] = Field(default_factory=list)


class HttpInstrumentation(BaseModel):
    """Transport-level HTTP instrumentation (header capture, sanitization).

    Collected per-worker. Use ``metrics`` to selectively disable
    individual HTTP server metrics::

        HttpInstrumentation(metrics=HttpMetrics(server_active_requests=False))
    """

    type: Literal["http"] = "http"
    enabled: bool = True
    capture_headers: CaptureHeaders = Field(default_factory=CaptureHeaders)
    metrics: HttpMetrics = Field(default_factory=HttpMetrics)


class SystemInstrumentation(BaseModel):
    """Machine-wide metrics instrumentation (CPU, memory, swap, disk, network).

    Collected on the supervisor only. System-level gauges are global
    to the machine and identical regardless of which process reads them,
    so a single collection task on the supervisor avoids redundant work.

    Note: the supervisor uses Rust-side defaults for these toggles
    and does not read this Python config (no Python interpreter in
    the supervisor process). This model is provided so the config
    schema is self-documenting and for future IPC-based relay.
    """

    type: Literal["system"] = "system"
    enabled: bool = True
    metrics: SystemMetrics = Field(default_factory=SystemMetrics)
    interval_seconds: float = Field(default=15.0, gt=0)


class ProcessInstrumentation(BaseModel):
    """Per-process metrics instrumentation (CPU, RSS, threads).

    Collected per-worker. Each worker spawns a background task that
    periodically reads its own process stats via ``sysinfo`` and
    reports them as OTEL gauges. The supervisor also collects its
    own process metrics independently (using Rust defaults).

    Attribution: OTEL Resource carries ``apx.worker.id`` for workers
    and ``apx.role=supervisor`` for the supervisor process.
    """

    type: Literal["process"] = "process"
    enabled: bool = True
    metrics: ProcessMetrics = Field(default_factory=ProcessMetrics)
    interval_seconds: float = Field(default=15.0, gt=0)


class ApxInstrumentation(BaseModel):
    """APX framework dispatch timing metrics (opt-in).

    Collected per-worker. Records per-phase histograms for the ASGI
    dispatch pipeline. All metrics default to disabled::

        ApxInstrumentation(metrics=ApxMetrics(dispatch_total=True))
    """

    type: Literal["apx"] = "apx"
    enabled: bool = True
    metrics: ApxMetrics = Field(default_factory=ApxMetrics)


def _instrumentation_type(v: Any) -> str:
    if isinstance(v, dict):
        return v.get("type", "")
    return getattr(v, "type", "")


Instrumentation = Annotated[
    Union[
        Annotated[HttpInstrumentation, Tag("http")],
        Annotated[SystemInstrumentation, Tag("system")],
        Annotated[ProcessInstrumentation, Tag("process")],
        Annotated[ApxInstrumentation, Tag("apx")],
    ],
    Discriminator(_instrumentation_type),
]

_DEFAULT_INSTRUMENTATIONS: list[Instrumentation] = [
    HttpInstrumentation(),
    SystemInstrumentation(),
    ProcessInstrumentation(),
]

if os.environ.get("APX_PERF"):
    _DEFAULT_INSTRUMENTATIONS.append(ApxInstrumentation())


class Configuration(BaseModel):
    """Telemetry instrumentation configuration.

    Defaults enable HTTP, system, and APX instrumentation automatically.
    Call ``configure()`` only to override specific instrumentations::

        from apx.telemetry import configure, Configuration, SystemInstrumentation

        configure(Configuration(
            instrumentations=[SystemInstrumentation(enabled=False)],
        ))
    """

    instrumentations: list[Instrumentation] = Field(default_factory=list)


_config: Configuration = Configuration()


def configure(config: Configuration) -> None:
    """Override the default telemetry configuration.

    User-provided instrumentations are merged with defaults by ``type``:
    same type replaces the default, new types are appended, omitted
    defaults are kept as-is.
    """
    global _config  # noqa: PLW0603
    _config = config


def _effective_instrumentations() -> list[Instrumentation]:
    """Merge user instrumentations with defaults by type key."""
    user_by_type: dict[str, Instrumentation] = {
        i.type: i for i in _config.instrumentations
    }
    result: list[Instrumentation] = []
    seen: set[str] = set()
    for default in _DEFAULT_INSTRUMENTATIONS:
        key = default.type
        seen.add(key)
        result.append(user_by_type.get(key, default))
    for key, instr in user_by_type.items():
        if key not in seen:
            result.append(instr)
    return result


def _get_config() -> dict[str, Any]:
    """Serialize the effective config (defaults + overrides) for Rust."""
    effective = _effective_instrumentations()
    return {
        "instrumentations": [i.model_dump() for i in effective],
    }
