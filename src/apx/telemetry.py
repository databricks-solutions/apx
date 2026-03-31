"""APX Telemetry — spans, metrics, and structured logs via OTLP.

Quick start::

    from apx.telemetry import span, log, Counter, Histogram, Gauge, Unit

    with span("db.query", table="users") as s:
        s.set_attribute("rows", "42")

    log.info("request handled", method="GET", status=200)

    counter = Counter("http.requests", unit=Unit.requests)
    counter.inc(1, attributes={"method": "GET"})

All telemetry is exported via OTLP gRPC and lands in three tables.


Table: spans
~~~~~~~~~~~~

| Column                  | Populated                                                          |
|-------------------------|--------------------------------------------------------------------|
| name                    | User: first arg — ``span("my.op")``                               |
| kind                    | User: ``kind=SpanKind.CLIENT`` (default ``INTERNAL``)              |
| attributes              | User: ``**kwargs`` on ``span()`` / ``set_attribute()`` on handle   |
| events                  | User: ``add_event()`` / ``record_exception()`` / auto on exception |
| status                  | User: ``set_status()`` / auto ``Error`` on exception               |
| trace_id                | Auto: inherited from parent or generated for root spans            |
| span_id                 | Auto: generated per span                                           |
| parent_span_id          | Auto: from enclosing ``span()`` context                            |
| trace_state             | Auto: propagated from parent context / incoming headers            |
| flags                   | Auto: trace flags from span context                                |
| start_time_unix_nano    | Auto: SDK timestamp on enter                                       |
| end_time_unix_nano      | Auto: SDK timestamp on exit                                        |
| links                   | Not yet exposed                                                    |
| dropped_*_count         | Auto: SDK overflow counters                                        |
| service_name            | Auto: from ``resource.attributes["service.name"]``                 |
| resource                | Config: ``Configuration(resource=Resource(...))``                  |
| resource_schema_url     | Auto: semconv schema URL                                           |
| instrumentation_scope   | Auto: ``apx.user`` + framework version                             |
| span_schema_url         | Auto: semconv schema URL                                           |


Table: logs
~~~~~~~~~~~

| Column                  | Populated                                                  |
|-------------------------|------------------------------------------------------------|
| body                    | User: message arg — ``log.info("message")``                |
| attributes              | User: ``**kwargs`` — ``log.info(..., method="GET")``       |
| event_name              | User: ``event_name="user.login"`` kwarg                    |
| severity_text           | Auto: from log level (``info``, ``warn``, ``error``, ...)  |
| severity_number         | Auto: from log level                                       |
| trace_id                | Auto: from active span context                             |
| span_id                 | Auto: from active span context                             |
| time_unix_nano          | Auto: SDK timestamp                                        |
| observed_time_unix_nano | Auto: SDK timestamp                                        |
| flags                   | Auto: trace flags from span context                        |
| dropped_attributes_count| Auto: SDK overflow counter                                 |
| service_name            | Auto: from ``resource.attributes["service.name"]``         |
| resource                | Config: ``Configuration(resource=Resource(...))``          |
| resource_schema_url     | Auto: semconv schema URL                                   |
| instrumentation_scope   | Auto: ``apx.python`` + framework version                   |
| log_schema_url          | Auto: semconv schema URL                                   |


Table: metrics
~~~~~~~~~~~~~~

| Column                  | Populated                                                           |
|-------------------------|---------------------------------------------------------------------|
| name                    | User: constructor arg — ``Counter("http.requests")``                |
| description             | User: ``description=`` kwarg on constructor                         |
| unit                    | User: ``unit=`` kwarg on constructor                                |
| metric_type             | Auto: ``Sum`` / ``Histogram`` / ``Gauge``                           |
| sum.attributes          | User: ``attributes=`` kwarg on ``Counter.inc()``                    |
| histogram.attributes    | User: ``attributes=`` kwarg on ``Histogram.observe()``              |
| gauge.attributes        | User: ``attributes=`` kwarg on ``Gauge.set()``                      |
| start_time_unix_nano    | Auto: SDK (null for Gauge per spec)                                 |
| time_unix_nano          | Auto: SDK collection timestamp                                      |
| metadata                | Not settable via SDK                                                |
| service_name            | Auto: from ``resource.attributes["service.name"]``                  |
| resource                | Config: ``Configuration(resource=Resource(...))``                   |
| resource_schema_url     | Auto: semconv schema URL                                            |
| instrumentation_scope   | Auto: ``apx.user`` + framework version                              |
| metric_schema_url       | Auto: semconv schema URL                                            |

Note: per-data-point attributes are nested inside the metric type struct
(``sum.attributes``, ``histogram.attributes``, ``gauge.attributes``).
There is no top-level ``attributes`` column on the metrics table.


resource vs attributes
~~~~~~~~~~~~~~~~~~~~~~

Two different columns across all three tables.

``resource.attributes`` describes the *entity* (process/service) — set once
at startup via ``Configuration``. ``attributes`` describes the *individual
event* — set per ``span()``/``log``/metric call.

Example::

    # Entity identity (shared across all signals)
    configure(Configuration(
        resource=Resource(attributes=[
            Attribute(key="deployment.environment", value="production"),
        ])
    ))

    # Per-event attributes
    log.info("handled", method="GET", status=200)
    counter.inc(1, attributes={"method": "GET"})
    with span("db.query", table="users"):
        ...
"""

from __future__ import annotations

import asyncio
import enum
import functools
import os
import sys
import traceback
from typing import Annotated, Any, Callable, ClassVar, Literal, TypeVar, Union

from pydantic import BaseModel, Discriminator, Field, Tag

from apx._core import (
    PyMetricDefinition as MetricDefinition,
    RustCounter,
    RustGauge,
    RustHistogram,
    SpanHandle,
    StatusCode,
    create_counter as _create_counter,
    create_gauge as _create_gauge,
    create_histogram as _create_histogram,
    metric_catalog,
)

_F = TypeVar("_F", bound=Callable[..., Any])


# ── Worker identity ──────────────────────────────────────────────────────
# Resolved once at import time from env vars set by the supervisor.
# These attributes are merged into every span and log-level span.


def _resolve_identity() -> dict[str, str]:
    worker_id = os.environ.get("APX_WORKER_ID")
    if worker_id is not None:
        return {"apx.process.type": "worker", "apx.worker.id": f"worker-{worker_id}"}
    return {"apx.process.type": "supervisor", "apx.worker.id": "supervisor"}


_IDENTITY_ATTRS: dict[str, str] = _resolve_identity()

__all__ = [
    "span",
    "log",
    "StatusCode",
    "SpanKind",
    "Unit",
    "Attribute",
    "Resource",
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
    "MetricDefinition",
    "metric_catalog",
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


# ── SpanKind ─────────────────────────────────────────────────────────────


class SpanKind(enum.IntEnum):
    """Span kind — populates the ``kind`` column in the spans table.

    Defaults to ``INTERNAL`` for application logic. Use ``SERVER`` /
    ``CLIENT`` for RPC boundaries, ``PRODUCER`` / ``CONSUMER`` for
    messaging.

    Example::

        with span("rpc.call", kind=SpanKind.CLIENT):
            ...
    """

    INTERNAL = 1
    SERVER = 2
    CLIENT = 3
    PRODUCER = 4
    CONSUMER = 5


# ── span ─────────────────────────────────────────────────────────────────


class span:
    """Context manager and decorator for creating trace spans.

    Writes to the spans table. See module docstring for the full column
    mapping.

    Examples::

        # Sync context manager
        with span("db.query", table="users") as s:
            s.set_attribute("rows", "42")
            s.add_event("cache_miss", attributes={"key": "user:1"})

        # Async context manager
        async with span("fetch_data", endpoint="/api") as s:
            result = await client.get("/api")

        # Decorator
        @span("load_user")
        async def load_user(uid: int): ...

        # Explicit kind for RPC boundaries
        with span("rpc.call", kind=SpanKind.CLIENT, service="auth"):
            ...

        # Manual status
        with span("validate") as s:
            if not valid:
                s.set_status(StatusCode.Error, "validation failed")

    Args:
        name: Span name — populates the ``name`` column.
        kind: Span kind — populates the ``kind`` column (default ``INTERNAL``).
        **attributes: Key-value pairs — populate the ``attributes`` column.
    """

    def __init__(
        self, name: str, *, kind: SpanKind = SpanKind.INTERNAL, **attributes: Any
    ) -> None:
        self._name = name
        self._kind = kind
        self._attributes = {k: str(v) for k, v in attributes.items()}
        self._handle: SpanHandle | None = None

    def _merged_attrs(self) -> dict[str, str]:
        return {**_IDENTITY_ATTRS, **self._attributes}

    def __enter__(self) -> SpanHandle:
        self._handle = SpanHandle(self._name, self._merged_attrs(), int(self._kind))
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
        self._handle = SpanHandle(self._name, self._merged_attrs(), int(self._kind))
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
                async with span(self._name, kind=self._kind, **self._attributes):
                    return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            with span(self._name, kind=self._kind, **self._attributes):
                return fn(*args, **kwargs)

        return sync_wrapper  # type: ignore[return-value]


# ── log ──────────────────────────────────────────────────────────────────


def _emit_log_span(
    level: str,
    message: str,
    *,
    event_name: str | None = None,
    **attributes: Any,
) -> None:
    """Create an instant (zero-duration) span representing a log event."""
    attrs = {**_IDENTITY_ATTRS, **{k: str(v) for k, v in attributes.items()}}
    attrs["log.level"] = level
    if event_name is not None:
        attrs["event.name"] = event_name
    handle = SpanHandle(message, attrs)
    handle.__enter__()
    handle.__exit__(None, None, None)


class _Log:
    """Structured logging — writes to the logs table.

    See module docstring for the full column mapping.

    Examples::

        from apx.telemetry import log

        log.info("request handled", method="GET", status=200)
        log.warn("slow query", duration_ms=1200)
        log.info("user signed in", event_name="user.login", uid="42")

        try:
            ...
        except Exception:
            log.exception("operation failed", event_name="op.error")
    """

    __slots__ = ()

    @staticmethod
    def trace(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit a TRACE-level log.

        Example::

            log.trace("cache lookup", key="user:1")
        """
        _emit_log_span("trace", message, event_name=event_name, **attributes)

    @staticmethod
    def debug(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit a DEBUG-level log.

        Example::

            log.debug("query plan", sql="SELECT ...", rows=42)
        """
        _emit_log_span("debug", message, event_name=event_name, **attributes)

    @staticmethod
    def info(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit an INFO-level log.

        Example::

            log.info("request handled", method="GET", status=200)
            log.info("user signed in", event_name="user.login", uid="42")
        """
        _emit_log_span("info", message, event_name=event_name, **attributes)

    @staticmethod
    def notice(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit a NOTICE-level log.

        Example::

            log.notice("rate limit approaching", usage_pct=85)
        """
        _emit_log_span("notice", message, event_name=event_name, **attributes)

    @staticmethod
    def warn(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit a WARN-level log.

        Example::

            log.warn("slow query", duration_ms=1200, query="SELECT ...")
        """
        _emit_log_span("warn", message, event_name=event_name, **attributes)

    @staticmethod
    def error(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit an ERROR-level log.

        Example::

            log.error("payment failed", event_name="payment.error", order_id="abc")
        """
        _emit_log_span("error", message, event_name=event_name, **attributes)

    @staticmethod
    def fatal(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit a FATAL-level log.

        Example::

            log.fatal("database unreachable", host="db-primary")
        """
        _emit_log_span("fatal", message, event_name=event_name, **attributes)

    @staticmethod
    def exception(message: str, *, event_name: str | None = None, **attributes: Any) -> None:
        """Emit an ERROR-level log with the current exception attached.

        Must be called from an ``except`` block. Automatically captures
        ``exception.type``, ``exception.message``, and
        ``exception.stacktrace`` as attributes.

        Example::

            try:
                process_payment(order)
            except Exception:
                log.exception("payment failed", event_name="payment.error")
        """
        exc_info = sys.exc_info()
        attrs = {**_IDENTITY_ATTRS, **{k: str(v) for k, v in attributes.items()}}
        attrs["log.level"] = "error"
        if event_name is not None:
            attrs["event.name"] = event_name
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
    """Monotonic sum metric — writes to the metrics table as ``metric_type=Sum``.

    Per-data-point attributes end up in ``sum.attributes``.
    See module docstring for the full column mapping.

    Example::

        counter = Counter("http.requests", description="Total requests", unit=Unit.requests)
        counter.inc()
        counter.inc(5, attributes={"method": "POST", "status": "200"})
    """

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustCounter = _create_counter(name, description, str(unit))

    def inc(self, value: int = 1, *, attributes: dict[str, str] | None = None) -> None:
        """Increment the counter.

        Example::

            counter.inc()
            counter.inc(5, attributes={"method": "POST"})
        """
        self._instrument.inc(value, attributes)


class Histogram:
    """Distribution metric — writes to the metrics table as ``metric_type=Histogram``.

    Per-data-point attributes end up in ``histogram.attributes``.
    See module docstring for the full column mapping.

    Example::

        histogram = Histogram("http.request.duration", description="Request latency", unit=Unit.milliseconds)
        histogram.observe(42.0)
        histogram.observe(120.5, attributes={"route": "/api/users"})
    """

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustHistogram = _create_histogram(
            name, description, str(unit)
        )

    def observe(self, value: float, *, attributes: dict[str, str] | None = None) -> None:
        """Record an observation.

        Example::

            histogram.observe(42.0)
            histogram.observe(120.5, attributes={"route": "/api/users"})
        """
        self._instrument.observe(value, attributes)


class Gauge:
    """Last-value metric — writes to the metrics table as ``metric_type=Gauge``.

    Per-data-point attributes end up in ``gauge.attributes``.
    ``start_time_unix_nano`` is null for gauges per OTLP spec.
    See module docstring for the full column mapping.

    Example::

        gauge = Gauge("db.connections", description="Active connections", unit=Unit.dimensionless)
        gauge.set(42.0)
        gauge.set(7.0, attributes={"pool": "primary"})
    """

    def __init__(
        self, name: str, *, description: str = "", unit: Unit | str = ""
    ) -> None:
        self.name = name
        self.description = description
        self.unit = unit
        self._instrument: RustGauge = _create_gauge(name, description, str(unit))

    def set(self, value: float, *, attributes: dict[str, str] | None = None) -> None:
        """Set the gauge value.

        Example::

            gauge.set(42.0)
            gauge.set(7.0, attributes={"pool": "primary"})
        """
        self._instrument.set(value, attributes)


# ── Instrumentation configuration ────────────────────────────────────────


class Attribute(BaseModel):
    """A key-value pair for ``resource.attributes`` configuration.

    Populates the ``resource`` column across all three tables.
    This is the process identity — distinct from per-event ``attributes``.

    Example::

        Attribute(key="deployment.environment", value="production")
    """

    key: str
    value: str


class Resource(BaseModel):
    """Resource identity — populates the ``resource`` column across all three tables.

    Attributes set here are merged with built-in attributes
    (``service.name``, ``apx.process.type``, ``apx.worker.id``,
    ``apx.app_path``) and environment variables (``OTEL_SERVICE_NAME``,
    ``OTEL_RESOURCE_ATTRIBUTES``). User attributes win on key collision.

    Example::

        Resource(attributes=[
            Attribute(key="deployment.environment", value="production"),
            Attribute(key="team", value="platform"),
        ])
    """

    attributes: list[Attribute] = Field(default_factory=list)
    schema_url: str | None = None


class ProcessMetrics(BaseModel):
    """Per-process metric toggles.

    Collected per-worker (and once for the supervisor process itself).
    Each process is identified by the OTEL Resource attributes
    ``apx.process.type`` (``"supervisor"`` or ``"worker"``) and
    ``apx.worker.id`` (``"supervisor"`` or ``"worker-N"``).

    Metric names, descriptions, and units are defined in the Rust
    metric definitions module (``telemetry/defs.rs``).
    """

    cpu: bool = True
    memory: bool = False
    threads: bool = False


class SystemMetrics(BaseModel):
    """Machine-wide metric toggles.

    Collected once on the supervisor process only. These are global
    system gauges (CPU, memory, paging, disk I/O, network I/O) that
    are identical regardless of which process reads them, so only
    the supervisor collects them to avoid N redundant copies.

    Metric names, descriptions, and units are defined in the Rust
    metric definitions module (``telemetry/defs.rs``).
    """

    cpu: bool = True
    memory: bool = True
    paging: bool = False
    disk_io: bool = False
    network_io: bool = False


class HttpMetrics(BaseModel):
    """HTTP server metric toggles.

    Collected per-worker. Each worker reports its own request duration
    and active request count. Use ``apx.worker.id`` to distinguish
    workers; aggregate across all workers at query time (e.g.
    ``sum(rate(...))``) for server-wide totals.
    """

    server_request_duration: bool = True
    server_active_requests: bool = True


class ApxMetrics(BaseModel):
    """APX framework dispatch pipeline metric toggles.

    Collected per-worker. Each histogram records latency for the
    dispatch phases within a single worker process. Use
    ``apx.worker.id`` to drill down; aggregate across workers for
    server-wide distributions.

    If APX_PERF environment variable is not set, none of these metrics are collected.
    """

    dispatch_body_collect: bool = True
    dispatch_crossbeam_send: bool = True
    dispatch_response_wait: bool = True
    dispatch_total: bool = True
    asgi_receive_build: bool = True
    asgi_send_parse: bool = True
    dispatch_pickup_delay: bool = True
    dispatch_materialize: bool = True
    dispatch_queue_depth: bool = True


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
    """Machine-wide metrics instrumentation (CPU, memory, paging, disk, network).

    Collected on the supervisor only via OTEL observable gauges. The SDK
    invokes registered callbacks at each export cycle, so no manual
    collection interval is needed.

    The first worker relays this configuration to the supervisor via IPC
    after loading the Python app, so user overrides are honoured.
    """

    type: Literal["system"] = "system"
    enabled: bool = True
    metrics: SystemMetrics = Field(default_factory=SystemMetrics)


class ProcessInstrumentation(BaseModel):
    """Per-process metrics instrumentation (CPU, RSS, threads).

    Collected per-worker and on the supervisor via OTEL observable gauges.
    The SDK invokes registered callbacks at each export cycle, so no
    manual collection interval is needed.

    Attribution: OTEL Resource carries ``apx.process.type``
    (``"supervisor"`` or ``"worker"``) and ``apx.worker.id``
    (``"supervisor"`` or ``"worker-N"``).
    """

    type: Literal["process"] = "process"
    enabled: bool = True
    metrics: ProcessMetrics = Field(default_factory=ProcessMetrics)


class ApxInstrumentation(BaseModel):
    """APX framework dispatch timing metrics (opt-in).

    Collected per-worker. Records per-phase histograms for the ASGI
    dispatch pipeline. If APX_PERF environment variable is not set, none of these metrics are collected::

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
    """Telemetry pipeline configuration.

    Defaults enable HTTP, system, and process instrumentation. Call
    ``configure()`` only to override specific instrumentations or to
    set custom ``resource`` attributes.

    Example::

        from apx.telemetry import configure, Configuration, Resource, Attribute

        configure(Configuration(
            resource=Resource(attributes=[
                Attribute(key="deployment.environment", value="staging"),
            ]),
            instrumentations=[SystemInstrumentation(enabled=False)],
        ))
    """

    resource: Resource = Field(default_factory=Resource)
    instrumentations: list[Instrumentation] = Field(default_factory=list)


_config: Configuration = Configuration()


def configure(config: Configuration) -> None:
    """Override the default telemetry configuration.

    User-provided instrumentations are merged with defaults by ``type``:
    same type replaces the default, new types are appended, omitted
    defaults are kept as-is.

    Example::

        configure(Configuration(
            resource=Resource(attributes=[
                Attribute(key="team", value="platform"),
            ]),
        ))
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


# note - unused in python but called from rust
def _get_config() -> dict[str, Any]:
    """Serialize the effective config (defaults + overrides) for Rust."""
    effective = _effective_instrumentations()
    resource = _config.resource
    resource_dict: dict[str, Any] = {
        "attributes": [{"key": a.key, "value": a.value} for a in resource.attributes],
    }
    if resource.schema_url is not None:
        resource_dict["schema_url"] = resource.schema_url
    return {
        "resource": resource_dict,
        "instrumentations": [i.model_dump() for i in effective],
    }
