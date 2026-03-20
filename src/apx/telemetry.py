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
    "ApxInstrumentation",
    "ApxMetrics",
    "Metric",
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


class Metric(BaseModel):
    """A single observable metric descriptor.

    Carries the OTEL metric name, a human-readable description, the logical
    group it belongs to (``"system"``, ``"http"``, or ``"apx"``), and whether
    it is enabled (``default=True``) or disabled (``default=False``)::

        SystemInstrumentation(metrics=SystemMetrics(
            system_disk_io=Metric(
                title="system.disk.io",
                description="Cumulative disk I/O in bytes",
                group="system",
                default=True,
            )
        ))
    """

    title: str
    description: str = ""
    group: str
    default: bool


class SystemMetrics(BaseModel):
    """Per-metric descriptors for system instrumentation.

    Override individual fields to enable or disable metrics::

        SystemInstrumentation(metrics=SystemMetrics(
            system_disk_io=SystemMetrics().system_disk_io.model_copy(update={"default": True})
        ))
    """

    process_cpu: Metric = Metric(
        title="process.cpu.utilization",
        description="APX worker process CPU utilization as a fraction of one core",
        group="system",
        default=True,
    )
    process_memory: Metric = Metric(
        title="process.memory.usage",
        description="APX worker process resident memory usage in bytes",
        group="system",
        default=False,
    )
    process_threads: Metric = Metric(
        title="process.thread.count",
        description="Number of threads in the APX worker process",
        group="system",
        default=False,
    )
    system_cpu: Metric = Metric(
        title="system.cpu.simple_utilization",
        description="System-wide CPU utilization as a fraction",
        group="system",
        default=True,
    )
    system_memory: Metric = Metric(
        title="system.memory.utilization",
        description="System memory utilization as a fraction",
        group="system",
        default=True,
    )
    system_swap: Metric = Metric(
        title="system.swap.utilization",
        description="System swap utilization as a fraction",
        group="system",
        default=False,
    )

    system_disk_io: Metric = Metric(
        title="system.disk.io",
        description="Cumulative disk I/O in bytes",
        group="system",
        default=False,
    )
    system_network_io: Metric = Metric(
        title="system.network.io",
        description="Cumulative network I/O in bytes",
        group="system",
        default=False,
    )


class HttpMetrics(BaseModel):
    """Per-metric descriptors for HTTP server instrumentation.

    All HTTP metrics are enabled by default.
    """

    server_request_duration: Metric = Metric(
        title="http.server.request.duration",
        description="HTTP server request duration",
        group="http",
        default=True,
    )
    server_active_requests: Metric = Metric(
        title="http.server.active_requests",
        description="Number of in-flight HTTP server requests",
        group="http",
        default=True,
    )


class ApxMetrics(BaseModel):
    """Per-metric descriptors for APX framework dispatch timing metrics.

    All dispatch metrics are disabled by default (low-overhead opt-in).
    """

    dispatch_body_collect: Metric = Metric(
        title="apx.dispatch.body_collect.duration",
        description="Time to collect the request body from the network stream",
        group="apx",
        default=False,
    )
    dispatch_crossbeam_send: Metric = Metric(
        title="apx.dispatch.crossbeam_send.duration",
        description="Time to send the request over the Crossbeam channel to the Python worker",
        group="apx",
        default=False,
    )
    dispatch_response_wait: Metric = Metric(
        title="apx.dispatch.response_wait.duration",
        description="Time waiting for the Python coroutine to produce the final response",
        group="apx",
        default=False,
    )
    dispatch_total: Metric = Metric(
        title="apx.dispatch.total.duration",
        description="Total end-to-end ASGI dispatch time",
        group="apx",
        default=False,
    )
    asgi_receive_build: Metric = Metric(
        title="apx.asgi.receive_build.duration",
        description="Time to build the ASGI receive dict",
        group="apx",
        default=False,
    )
    asgi_send_parse: Metric = Metric(
        title="apx.asgi.send_parse.duration",
        description="Time to parse an ASGI send event",
        group="apx",
        default=False,
    )


class CaptureHeaders(BaseModel):
    """HTTP header capture rules."""

    request: list[str] = Field(default_factory=list)
    response: list[str] = Field(default_factory=list)
    sanitize: list[str] = Field(default_factory=list)


class HttpInstrumentation(BaseModel):
    """Transport-level HTTP instrumentation (header capture, sanitization).

    Use ``metrics`` to selectively disable individual HTTP server metrics::

        HttpInstrumentation(metrics=HttpMetrics(
            server_active_requests=Metric(
                title="http.server.active_requests",
                description="Number of in-flight HTTP server requests",
                group="http",
                default=False,
            )
        ))
    """

    type: Literal["http"] = "http"
    enabled: bool = True
    capture_headers: CaptureHeaders = Field(default_factory=CaptureHeaders)
    metrics: HttpMetrics = Field(default_factory=HttpMetrics)


class SystemInstrumentation(BaseModel):
    """System metrics collection (CPU, memory, disk, network).

    Use ``metrics`` to selectively enable individual system metrics::

        SystemInstrumentation(metrics=SystemMetrics(
            system_disk_io=Metric(
                title="system.disk.io",
                description="Cumulative disk I/O in bytes",
                group="system",
                default=True,
            )
        ))
    """

    type: Literal["system"] = "system"
    enabled: bool = True
    metrics: SystemMetrics = Field(default_factory=SystemMetrics)
    interval_seconds: float = Field(default=15.0, gt=0)


class ApxInstrumentation(BaseModel):
    """APX framework dispatch timing metrics (opt-in).

    Records per-phase histograms for the ASGI dispatch pipeline.
    All metrics default to disabled — enable selectively::

        ApxInstrumentation(metrics=ApxMetrics(
            dispatch_total=Metric(
                title="apx.dispatch.total.duration",
                description="Total end-to-end ASGI dispatch time",
                group="apx",
                default=True,
            )
        ))
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
        Annotated[ApxInstrumentation, Tag("apx")],
    ],
    Discriminator(_instrumentation_type),
]

_DEFAULT_INSTRUMENTATIONS: list[Instrumentation] = [
    HttpInstrumentation(),
    SystemInstrumentation(),
]

if os.environ.get("APX_PERF"):
    _DEFAULT_INSTRUMENTATIONS.append(ApxInstrumentation())


class Configuration(BaseModel):
    """Telemetry instrumentation configuration.

    Defaults enable HTTP, FastAPI, system, and APX instrumentation automatically.
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
