"""Integration tests for OTEL telemetry via Docker OTEL Collector.

Starts an official OpenTelemetry Collector container that receives OTLP/gRPC
and writes traces/metrics/logs to JSONL files.  An APX container exports
telemetry to this collector.  Tests parse the JSONL via Pydantic models and
verify signal correctness, trace context propagation, and resource attributes.
"""

from __future__ import annotations

import platform
import socket
import textwrap
import time
import uuid
from pathlib import Path
from typing import Generator, Literal

import docker
import docker.errors
import docker.models.containers
import httpx
import pytest

from .otlp_models import (
    InstrumentationScope,
    LogRecord,
    LogsExport,
    Metric,
    MetricsExport,
    Span,
    TracesExport,
    read_jsonl,
)

CONTAINER_NAME = "apx-telemetry-test"
COLLECTOR_CONTAINER_NAME = "apx-otel-collector-test"
COLLECTOR_IMAGE = "otel/opentelemetry-collector:0.120.0"

REQUEST_ID = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
REQUEST_ID_2 = "f0e1d2c3-b4a5-4968-87f6-e5d4c3b2a1f0"


# ---------------------------------------------------------------------------
# OTEL Collector wrapper
# ---------------------------------------------------------------------------


class OtelCollector:
    """Wraps a Dockerized OTEL Collector exporting to JSONL files."""

    def __init__(
        self,
        port: int,
        data_dir: Path,
        container: docker.models.containers.Container,
    ) -> None:
        self.port = port
        self.data_dir = data_dir
        self.container = container

    def traces(self) -> list[TracesExport]:
        return read_jsonl(self.data_dir / "traces.jsonl", TracesExport)

    def metrics(self) -> list[MetricsExport]:
        return read_jsonl(self.data_dir / "metrics.jsonl", MetricsExport)

    def logs(self) -> list[LogsExport]:
        return read_jsonl(self.data_dir / "logs.jsonl", LogsExport)

    def stop(self) -> None:
        self.container.stop(timeout=5)
        self.container.remove()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def _wait_for_healthy(base_url: str, *, timeout: float = 120) -> None:
    """Poll the health endpoint until the container is ready."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            r = httpx.get(f"{base_url}/api/health", timeout=2.0)
            if r.status_code == 200:
                return
        except (httpx.ConnectError, httpx.ReadError, httpx.TimeoutException):
            pass
        time.sleep(1.0)
    pytest.fail(f"Container did not become healthy within {timeout}s (url={base_url})")


def _print_container_logs(
    container: docker.models.containers.Container,
    *,
    tail: int | Literal["all"] = "all",
    header: str = "Container logs",
) -> None:
    try:
        logs = container.logs(tail=tail).decode("utf-8", errors="replace")
    except Exception:
        return
    sep = "=" * 72
    print(f"\n{sep}\n  {header}\n{sep}\n{logs}\n{sep}")


def _generate_telemetry(
    client: httpx.Client,
    *,
    request_id: str | None = None,
) -> httpx.Response:
    """Hit the telemetry test endpoint, optionally with a specific request id."""
    headers = {}
    if request_id is not None:
        headers["x-request-id"] = request_id
    r = client.get("/api/telemetry/test", headers=headers)
    assert r.status_code == 200
    assert r.json() == {"ok": True}
    return r


def _wait_for_collector_data(
    collector: OtelCollector,
    *,
    timeout: float = 30,
    require_logs: bool = False,
) -> None:
    """Wait until the collector has received at least some traces and metrics.

    When *require_logs* is True, also waits for at least one log record.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        has_traces = len(collector.traces()) > 0
        has_metrics = len(collector.metrics()) > 0
        has_logs = not require_logs or len(collector.logs()) > 0
        if has_traces and has_metrics and has_logs:
            return
        time.sleep(1.0)


def _uuid_to_trace_id(uid: str) -> str:
    """Convert a UUID string to the OTEL hex trace-id (32 hex chars, no dashes)."""
    return uuid.UUID(uid).bytes.hex()


def _flat_log_records(
    collector: OtelCollector,
) -> list[tuple[InstrumentationScope, LogRecord]]:
    """Flatten all (scope, log_record) pairs from the collector."""
    records: list[tuple[InstrumentationScope, LogRecord]] = []
    for export in collector.logs():
        for rl in export.resourceLogs:
            for sl in rl.scopeLogs:
                for lr in sl.logRecords:
                    records.append((sl.scope, lr))
    return records


def _flat_metrics(collector: OtelCollector) -> list[Metric]:
    """Flatten all Metric objects from the collector."""
    metrics: list[Metric] = []
    for export in collector.metrics():
        for rm in export.resourceMetrics:
            for sm in rm.scopeMetrics:
                metrics.extend(sm.metrics)
    return metrics


def _flat_spans(collector: OtelCollector) -> list[Span]:
    """Flatten all Span objects from the collector."""
    spans: list[Span] = []
    for export in collector.traces():
        for rs in export.resourceSpans:
            for ss in rs.scopeSpans:
                spans.extend(ss.spans)
    return spans


def _metric_type(m: Metric) -> str:
    """Return the aggregation type of a metric: 'sum', 'histogram', or 'gauge'."""
    if m.sum is not None:
        return "sum"
    if m.histogram is not None:
        return "histogram"
    if m.gauge is not None:
        return "gauge"
    return "unknown"


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def otel_collector(
    tmp_path_factory: pytest.TempPathFactory,
) -> Generator[OtelCollector]:
    """Start an OTEL Collector Docker container writing JSONL to a temp dir."""
    data_dir = tmp_path_factory.mktemp("otel")
    port = _find_free_port()

    config_yaml = textwrap.dedent("""\
        receivers:
          otlp:
            protocols:
              grpc:
                endpoint: "0.0.0.0:4317"
        exporters:
          file/traces:
            path: /data/traces.jsonl
          file/metrics:
            path: /data/metrics.jsonl
          file/logs:
            path: /data/logs.jsonl
        service:
          pipelines:
            traces:
              receivers: [otlp]
              exporters: [file/traces]
            metrics:
              receivers: [otlp]
              exporters: [file/metrics]
            logs:
              receivers: [otlp]
              exporters: [file/logs]
    """)

    config_path = data_dir / "config.yaml"
    config_path.write_text(config_yaml)

    dk = docker.from_env()

    try:
        stale = dk.containers.get(COLLECTOR_CONTAINER_NAME)
        stale.remove(force=True)
    except docker.errors.NotFound:
        pass

    is_linux = platform.system() == "Linux"
    extra_hosts: dict[str, str] = {}
    if is_linux:
        extra_hosts["host.docker.internal"] = "host-gateway"

    container = dk.containers.run(
        COLLECTOR_IMAGE,
        name=COLLECTOR_CONTAINER_NAME,
        ports={"4317/tcp": port},
        volumes={
            str(data_dir): {"bind": "/data", "mode": "rw"},
            str(config_path): {"bind": "/etc/otelcol/config.yaml", "mode": "ro"},
        },
        extra_hosts=extra_hosts or None,
        detach=True,
    )

    print(f"[otel] Collector container started on port {port}, data_dir={data_dir}")
    time.sleep(3)

    collector = OtelCollector(port=port, data_dir=data_dir, container=container)
    yield collector

    _print_container_logs(container, tail=40, header="OTEL Collector logs (teardown)")
    collector.stop()


@pytest.fixture(scope="module")
def telemetry_container(
    apx_image: str,
    otel_collector: OtelCollector,
) -> Generator[str]:
    """Start an APX container with OTEL env vars pointing at the collector."""
    dk = docker.from_env()

    try:
        stale = dk.containers.get(CONTAINER_NAME)
        stale.remove(force=True)
    except docker.errors.NotFound:
        pass

    is_linux = platform.system() == "Linux"
    extra_hosts: dict[str, str] = {}
    if is_linux:
        extra_hosts["host.docker.internal"] = "host-gateway"

    endpoint = f"http://host.docker.internal:{otel_collector.port}"

    print(f"[telemetry] Starting APX container with OTEL endpoint={endpoint}")
    container = dk.containers.run(
        apx_image,
        command=["apx", "serve", "app.main", "--host", "0.0.0.0", "--workers", "1"],
        name=CONTAINER_NAME,
        platform="linux/amd64",
        ports={"8000/tcp": None},
        environment={
            "APX_LOG": "debug",
            "OTEL_EXPORTER_OTLP_ENDPOINT": endpoint,
            "OTEL_EXPORTER_OTLP_PROTOCOL": "grpc",
            "OTEL_SERVICE_NAME": "apx-integration-test",
            "OTEL_RESOURCE_ATTRIBUTES": "workspace.id=test-ws,app.name=bench-apx",
            "OTEL_BSP_SCHEDULE_DELAY": "500",
            "OTEL_BLRP_SCHEDULE_DELAY": "500",
            "OTEL_BSP_MAX_EXPORT_BATCH_SIZE": "16",
            "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE": "16",
        },
        extra_hosts=extra_hosts or None,
        detach=True,
    )

    container.reload()
    host_port = container.ports["8000/tcp"][0]["HostPort"]
    base_url = f"http://localhost:{host_port}"
    print(f"[telemetry] Container mapped to {base_url}")

    try:
        _wait_for_healthy(base_url)
    except Exception:
        _print_container_logs(
            container, header="Telemetry container logs (startup failed)"
        )
        container.stop(timeout=5)
        container.remove()
        raise

    print(f"[telemetry] Container healthy at {base_url}")

    yield base_url

    _print_container_logs(
        container, tail=40, header="Telemetry container logs (teardown)"
    )
    container.stop(timeout=10)
    container.remove()


@pytest.fixture(scope="module")
def telemetry_client(telemetry_container: str) -> Generator[httpx.Client]:
    """httpx client pointed at the telemetry-enabled APX container."""
    with httpx.Client(base_url=telemetry_container, timeout=30.0) as c:
        yield c


# ---------------------------------------------------------------------------
# Tests — existing signal verification
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestTelemetry:
    """Verify OTLP export of traces, metrics, and logs."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        """Generate telemetry once for all tests in this class."""
        _generate_telemetry(telemetry_client)
        _generate_telemetry(telemetry_client)
        _wait_for_collector_data(otel_collector)

    def test_traces_collected(self, otel_collector: OtelCollector) -> None:
        """HTTP request spans and custom SpanHandle spans arrive."""
        all_span_names: set[str] = set()
        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for span in ss.spans:
                        all_span_names.add(span.name)

        assert "test.custom_span" in all_span_names, (
            f"expected 'test.custom_span' in exported spans; got {all_span_names}"
        )

    def test_metrics_collected(self, otel_collector: OtelCollector) -> None:
        """HTTP and custom metrics (counter, histogram, gauge) arrive."""
        all_metric_names: set[str] = set()
        for export in otel_collector.metrics():
            for rm in export.resourceMetrics:
                for sm in rm.scopeMetrics:
                    for m in sm.metrics:
                        all_metric_names.add(m.name)

        assert "http.server.request.duration" in all_metric_names, (
            f"expected 'http.server.request.duration'; got {all_metric_names}"
        )
        assert "test.custom_counter" in all_metric_names, (
            f"expected 'test.custom_counter'; got {all_metric_names}"
        )
        assert "test.custom_histogram" in all_metric_names, (
            f"expected 'test.custom_histogram'; got {all_metric_names}"
        )
        assert "test.custom_gauge" in all_metric_names, (
            f"expected 'test.custom_gauge'; got {all_metric_names}"
        )

    def test_logs_collected(self, otel_collector: OtelCollector) -> None:
        """Python log messages forwarded via tracing arrive as OTLP logs."""
        all_log_bodies: list[str] = []
        for export in otel_collector.logs():
            for rl in export.resourceLogs:
                for sl in rl.scopeLogs:
                    for lr in sl.logRecords:
                        if lr.body.stringValue:
                            all_log_bodies.append(lr.body.stringValue)

        assert any("integration test log message" in b for b in all_log_bodies), (
            f"expected log containing 'integration test log message'; got {all_log_bodies[:20]}"
        )

    def test_log_level_spans_collected(self, otel_collector: OtelCollector) -> None:
        """log.info() produces an instant span with log.level attribute."""
        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for s in ss.spans:
                        if s.name == "integration test log message":
                            attrs = {a.key: a.value.stringValue for a in s.attributes}
                            assert attrs.get("log.level") == "info", (
                                f"expected log.level='info'; got {attrs}"
                            )
                            return

        all_span_names = {
            s.name
            for export in otel_collector.traces()
            for rs in export.resourceSpans
            for ss in rs.scopeSpans
            for s in ss.spans
        }
        pytest.fail(
            f"expected span 'integration test log message'; got {all_span_names}"
        )

    def test_resource_attributes(self, otel_collector: OtelCollector) -> None:
        """Resource carries service.name and custom attributes."""
        traces = otel_collector.traces()
        assert traces, "no trace data received"

        resource = traces[0].resourceSpans[0].resource
        attrs = {a.key: a.value.stringValue for a in resource.attributes}

        assert attrs.get("service.name") == "apx-integration-test", (
            f"expected service.name='apx-integration-test'; got {attrs}"
        )
        assert attrs.get("workspace.id") == "test-ws", (
            f"expected workspace.id='test-ws'; got {attrs}"
        )
        assert attrs.get("app.name") == "bench-apx", (
            f"expected app.name='bench-apx'; got {attrs}"
        )


# ---------------------------------------------------------------------------
# Tests — trace context propagation
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestTraceContext:
    """Verify x-request-id trace propagation and span/log correlation."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        """Generate telemetry with known request IDs for trace context tests."""
        _generate_telemetry(telemetry_client, request_id=REQUEST_ID)
        _generate_telemetry(telemetry_client, request_id=REQUEST_ID_2)
        time.sleep(5)
        _wait_for_collector_data(otel_collector)

    def test_http_span_uses_request_id_as_trace_id(
        self, otel_collector: OtelCollector
    ) -> None:
        """The root HTTP span's traceId matches the x-request-id UUID bytes."""
        expected = _uuid_to_trace_id(REQUEST_ID)
        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for span in ss.spans:
                        if span.traceId == expected:
                            return

        all_trace_ids = {
            s.traceId
            for export in otel_collector.traces()
            for rs in export.resourceSpans
            for ss in rs.scopeSpans
            for s in ss.spans
        }
        pytest.fail(
            f"expected traceId={expected} from x-request-id={REQUEST_ID}; "
            f"got {all_trace_ids}"
        )

    def test_python_spans_are_children_of_http_span(
        self, otel_collector: OtelCollector
    ) -> None:
        """Python SpanHandle spans share traceId with the HTTP root span."""
        expected_trace = _uuid_to_trace_id(REQUEST_ID)
        http_span_id: str | None = None
        custom_span_trace: str | None = None
        custom_span_parent: str | None = None

        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for span in ss.spans:
                        if span.traceId != expected_trace:
                            continue
                        if span.name == "http.server.request":
                            http_span_id = span.spanId
                        if span.name == "test.custom_span":
                            custom_span_trace = span.traceId
                            custom_span_parent = span.parentSpanId

        assert http_span_id is not None, "http.server.request span not found"
        assert custom_span_trace == expected_trace, (
            f"test.custom_span traceId mismatch: {custom_span_trace} != {expected_trace}"
        )
        assert custom_span_parent is not None, "test.custom_span has no parent"

    def test_log_spans_inherit_trace_context(
        self, otel_collector: OtelCollector
    ) -> None:
        """log.info() instant spans share the same traceId as the HTTP span."""
        expected_trace = _uuid_to_trace_id(REQUEST_ID)
        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for span in ss.spans:
                        if (
                            span.name == "integration test log message"
                            and span.traceId == expected_trace
                        ):
                            return

        all_log_spans = {
            (s.name, s.traceId)
            for export in otel_collector.traces()
            for rs in export.resourceSpans
            for ss in rs.scopeSpans
            for s in ss.spans
            if s.name == "integration test log message"
        }
        pytest.fail(
            f"expected log span with traceId={expected_trace}; got {all_log_spans}"
        )

    def test_otel_logs_have_trace_context(self, otel_collector: OtelCollector) -> None:
        """OTLP log records carry a non-empty traceId from the request span."""
        expected_trace = _uuid_to_trace_id(REQUEST_ID)
        for export in otel_collector.logs():
            for rl in export.resourceLogs:
                for sl in rl.scopeLogs:
                    for lr in sl.logRecords:
                        body = lr.body.stringValue or ""
                        if (
                            "integration test log message" in body
                            and lr.traceId == expected_trace
                        ):
                            return

        matching_logs = [
            (lr.body.stringValue, lr.traceId)
            for export in otel_collector.logs()
            for rl in export.resourceLogs
            for sl in rl.scopeLogs
            for lr in sl.logRecords
            if lr.body.stringValue
            and "integration test log message" in lr.body.stringValue
        ]
        pytest.fail(
            f"expected log with traceId={expected_trace}; "
            f"matching logs: {matching_logs[:10]}"
        )

    def test_different_requests_get_different_traces(
        self, otel_collector: OtelCollector
    ) -> None:
        """Two requests with distinct x-request-id produce distinct traceIds."""
        trace_1 = _uuid_to_trace_id(REQUEST_ID)
        trace_2 = _uuid_to_trace_id(REQUEST_ID_2)

        found_trace_ids: set[str] = set()
        for export in otel_collector.traces():
            for rs in export.resourceSpans:
                for ss in rs.scopeSpans:
                    for span in ss.spans:
                        if span.traceId in (trace_1, trace_2):
                            found_trace_ids.add(span.traceId)

        assert trace_1 in found_trace_ids, (
            f"traceId for REQUEST_ID not found: {trace_1}"
        )
        assert trace_2 in found_trace_ids, (
            f"traceId for REQUEST_ID_2 not found: {trace_2}"
        )
        assert trace_1 != trace_2


# ---------------------------------------------------------------------------
# Tests — log-trace correlation and log record completeness
# ---------------------------------------------------------------------------

ZERO_TRACE_ID = "0" * 32
ZERO_SPAN_ID = "0" * 16
REQUEST_ID_LOG = "b1c2d3e4-f5a6-4b7c-8d9e-0f1a2b3c4d5e"


@pytest.mark.integration
class TestLogTraceCorrelation:
    """Verify OTLP log records carry trace context and have complete metadata.

    Exercises both log emission paths:

    1. **Direct OTEL path** — Python ``logging.getLogger().info()`` goes through
       ``_emit_log`` → ``emit_otel_log_record``, which explicitly sets
       ``trace_id``/``span_id`` from the Python ``ContextVar``.  Scope: ``apx.python``.

    2. **Bridge path** — Rust ``tracing::info!`` events are converted to OTLP
       log records by ``OpenTelemetryTracingBridge``.  The bridge should read
       the active ``tracing::Span`` context on the tokio thread.
    """

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        _generate_telemetry(telemetry_client, request_id=REQUEST_ID_LOG)
        time.sleep(5)
        _wait_for_collector_data(otel_collector, require_logs=True)

    # ── Direct-path (Python) log tests ────────────────────────────────────

    def test_python_direct_logs_have_trace_context(
        self, otel_collector: OtelCollector
    ) -> None:
        """Python logging → emit_otel_log_record carries the request's traceId."""
        expected_trace = _uuid_to_trace_id(REQUEST_ID_LOG)
        records = _flat_log_records(otel_collector)

        direct_logs = [
            (scope, lr)
            for scope, lr in records
            if lr.body.stringValue
            and "integration test log message" in lr.body.stringValue
            and scope.name == "apx.python"
            and lr.traceId == expected_trace
        ]

        assert direct_logs, (
            f"no direct-path Python log records with trace {expected_trace} found; "
            f"all apx.python traces: "
            f"{[lr.traceId for s, lr in records if s.name == 'apx.python' and lr.body.stringValue and 'integration test log message' in lr.body.stringValue]}"
        )

        for _scope, lr in direct_logs:
            assert lr.spanId and lr.spanId != ZERO_SPAN_ID, (
                f"Python direct log should have non-empty spanId; got {lr.spanId!r}"
            )

    def test_python_direct_logs_have_complete_metadata(
        self, otel_collector: OtelCollector
    ) -> None:
        """Direct-path log records have severity, body, and timestamps."""
        records = _flat_log_records(otel_collector)

        direct_logs = [lr for scope, lr in records if scope.name == "apx.python"]
        assert direct_logs, "no direct-path Python log records found"

        for lr in direct_logs:
            body = (lr.body.stringValue or "")[:80]
            assert lr.body.stringValue, "direct log has empty body"
            assert lr.severityNumber > 0, (
                f"direct log has zero severityNumber: {body!r}"
            )
            assert lr.severityText, f"direct log missing severityText: {body!r}"
            assert lr.timeUnixNano and lr.timeUnixNano != "0", (
                f"direct log has zero timeUnixNano: {body!r}"
            )

    # ── No-duplicate tests ─────────────────────────────────────────────────

    def test_python_logs_not_duplicated(self, otel_collector: OtelCollector) -> None:
        """Each Python stdlib log should produce exactly one OTEL log record per request.

        Before the fix, ``emit_log()`` emitted both a ``tracing`` event (picked
        up by the ``OpenTelemetryTracingBridge``) AND a direct ``LogRecord``.
        The bridge record had no trace context, creating a confusing duplicate.
        Now the bridge filters ``apx::python`` events, so only the direct record
        (with scope ``apx.python``) should exist for a given trace.
        """
        expected_trace = _uuid_to_trace_id(REQUEST_ID_LOG)
        records = _flat_log_records(otel_collector)

        python_test_logs = [
            (scope, lr)
            for scope, lr in records
            if lr.body.stringValue
            and "integration test log message" in lr.body.stringValue
            and lr.traceId == expected_trace
        ]

        assert python_test_logs, (
            f"no 'integration test log message' logs for trace {expected_trace}"
        )

        scopes = [scope.name for scope, _ in python_test_logs]
        assert all(s == "apx.python" for s in scopes), (
            f"expected all Python test logs to come from scope 'apx.python'; "
            f"got scopes: {scopes}"
        )

        assert len(python_test_logs) == 1, (
            f"expected exactly 1 OTEL log record for trace {expected_trace}; "
            f"got {len(python_test_logs)} (scopes: {scopes})"
        )

    # ── Bridge-path (Rust tracing) log tests ──────────────────────────────

    def test_rust_http_log_has_trace_context(
        self, otel_collector: OtelCollector
    ) -> None:
        """Rust tracing::info! inside .instrument(span) carries traceId via the bridge.

        The ``record_duration`` function in ``http.rs`` emits
        ``"http metrics: first request duration recorded"`` inside the HTTP span
        on the tokio thread.  The ``OpenTelemetryTracingBridge`` should propagate
        the active span's trace context to the OTLP log record.
        """
        records = _flat_log_records(otel_collector)

        rust_logs = [
            (scope, lr)
            for scope, lr in records
            if lr.body.stringValue and "http metrics" in lr.body.stringValue
        ]

        assert rust_logs, (
            f"no 'http metrics' log found; "
            f"all log bodies: "
            f"{[lr.body.stringValue[:80] for _, lr in records if lr.body.stringValue]}"
        )

        for _scope, lr in rust_logs:
            assert lr.traceId and lr.traceId != ZERO_TRACE_ID, (
                f"Rust tracing::info! inside .instrument(span) should carry traceId "
                f"via OpenTelemetryTracingBridge; got traceId={lr.traceId!r}"
            )

    # ── Cross-cutting log metadata tests ──────────────────────────────────

    def test_all_log_records_have_severity(self, otel_collector: OtelCollector) -> None:
        """Every OTLP log record has non-zero severityNumber and non-empty severityText."""
        records = _flat_log_records(otel_collector)
        assert records, "no log records found in collector"

        for scope, lr in records:
            body = (lr.body.stringValue or "")[:80]
            assert lr.severityNumber > 0, (
                f"log record has zero severityNumber: body={body!r} scope={scope.name}"
            )
            assert lr.severityText, (
                f"log record missing severityText: body={body!r} scope={scope.name}"
            )

    def test_all_log_records_have_body(self, otel_collector: OtelCollector) -> None:
        """Every OTLP log record has a non-empty body."""
        records = _flat_log_records(otel_collector)
        assert records, "no log records found in collector"

        for scope, lr in records:
            assert lr.body.stringValue, (
                f"log record has empty body: scope={scope.name} traceId={lr.traceId}"
            )

    def test_all_log_records_have_timestamp(
        self, otel_collector: OtelCollector
    ) -> None:
        """Every OTLP log record has at least one valid timestamp.

        ``timeUnixNano`` is the event time (may be unset by the bridge).
        ``observedTimeUnixNano`` is when the SDK first saw the record.
        At least one must be present.
        """
        records = _flat_log_records(otel_collector)
        assert records, "no log records found in collector"

        for scope, lr in records:
            body = (lr.body.stringValue or "")[:80]
            has_time = bool(lr.timeUnixNano and lr.timeUnixNano != "0")
            has_observed = bool(
                lr.observedTimeUnixNano and lr.observedTimeUnixNano != "0"
            )
            assert has_time or has_observed, (
                f"log record has no timestamp: body={body!r} scope={scope.name}"
            )


# ---------------------------------------------------------------------------
# Tests — metric correctness (names, types, units, descriptions)
# ---------------------------------------------------------------------------

OTEL_SPAN_KIND_SERVER = 2


@pytest.mark.integration
class TestMetricCorrectness:
    """Verify exported metrics carry correct names, aggregation types, units, and descriptions.

    Guards against regressions like the histogram_bucket_view bug that
    replaced metric names with empty strings.
    """

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        _generate_telemetry(telemetry_client)
        _generate_telemetry(telemetry_client)
        _wait_for_collector_data(otel_collector)

    # ── Regression: no empty metric names ─────────────────────────────────

    def test_no_empty_metric_names(self, otel_collector: OtelCollector) -> None:
        """Regression: histogram_bucket_view must preserve instrument name."""
        metrics = _flat_metrics(otel_collector)
        assert metrics, "no metrics found in collector"

        empty = [m for m in metrics if not m.name]
        assert empty == [], (
            f"found {len(empty)} metric(s) with empty name; "
            f"all names: {sorted(set(m.name for m in metrics))}"
        )

    # ── Built-in metric names ─────────────────────────────────────────────

    def test_builtin_http_duration_present(self, otel_collector: OtelCollector) -> None:
        names = {m.name for m in _flat_metrics(otel_collector)}
        assert "http.server.request.duration" in names, (
            f"expected 'http.server.request.duration'; got {names}"
        )

    def test_builtin_http_active_requests_present(
        self, otel_collector: OtelCollector
    ) -> None:
        names = {m.name for m in _flat_metrics(otel_collector)}
        assert "http.server.active_requests" in names, (
            f"expected 'http.server.active_requests'; got {names}"
        )

    # ── Custom metric names ───────────────────────────────────────────────

    def test_custom_counter_present(self, otel_collector: OtelCollector) -> None:
        names = {m.name for m in _flat_metrics(otel_collector)}
        assert "test.custom_counter" in names

    def test_custom_histogram_present(self, otel_collector: OtelCollector) -> None:
        names = {m.name for m in _flat_metrics(otel_collector)}
        assert "test.custom_histogram" in names

    def test_custom_gauge_present(self, otel_collector: OtelCollector) -> None:
        names = {m.name for m in _flat_metrics(otel_collector)}
        assert "test.custom_gauge" in names

    # ── Aggregation types ─────────────────────────────────────────────────

    def test_http_duration_is_histogram(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.request.duration":
                assert _metric_type(m) == "histogram", (
                    f"http.server.request.duration should be histogram, got {_metric_type(m)}"
                )
                return
        pytest.fail("http.server.request.duration not found")

    def test_http_active_requests_is_sum(self, otel_collector: OtelCollector) -> None:
        """http.server.active_requests is an UpDownCounter → exported as sum."""
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.active_requests":
                assert _metric_type(m) in ("sum", "gauge"), (
                    f"http.server.active_requests should be sum or gauge, "
                    f"got {_metric_type(m)}"
                )
                return
        pytest.fail("http.server.active_requests not found")

    def test_custom_counter_is_sum(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_counter":
                assert _metric_type(m) == "sum", (
                    f"test.custom_counter should be sum, got {_metric_type(m)}"
                )
                return
        pytest.fail("test.custom_counter not found")

    def test_custom_histogram_is_histogram(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_histogram":
                assert _metric_type(m) == "histogram", (
                    f"test.custom_histogram should be histogram, got {_metric_type(m)}"
                )
                return
        pytest.fail("test.custom_histogram not found")

    def test_custom_gauge_is_gauge(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_gauge":
                assert _metric_type(m) == "gauge", (
                    f"test.custom_gauge should be gauge, got {_metric_type(m)}"
                )
                return
        pytest.fail("test.custom_gauge not found")

    # ── Units ─────────────────────────────────────────────────────────────

    def test_http_duration_unit_is_seconds(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.request.duration":
                assert m.unit == "s", (
                    f"http.server.request.duration unit should be 's', got {m.unit!r}"
                )
                return
        pytest.fail("http.server.request.duration not found")

    def test_http_active_requests_unit(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.active_requests":
                assert m.unit == "1", (
                    f"http.server.active_requests unit should be '1', got {m.unit!r}"
                )
                return
        pytest.fail("http.server.active_requests not found")

    def test_custom_counter_unit(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_counter":
                assert m.unit == "1", (
                    f"test.custom_counter unit should be '1', got {m.unit!r}"
                )
                return
        pytest.fail("test.custom_counter not found")

    def test_custom_histogram_unit(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_histogram":
                assert m.unit == "ms", (
                    f"test.custom_histogram unit should be 'ms', got {m.unit!r}"
                )
                return
        pytest.fail("test.custom_histogram not found")

    # ── Descriptions ──────────────────────────────────────────────────────

    def test_http_duration_description(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.request.duration":
                assert m.description, (
                    "http.server.request.duration should have non-empty description"
                )
                return
        pytest.fail("http.server.request.duration not found")

    def test_custom_counter_description(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_counter":
                assert m.description == "integration test counter", (
                    f"test.custom_counter description mismatch: {m.description!r}"
                )
                return
        pytest.fail("test.custom_counter not found")

    def test_custom_histogram_description(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_histogram":
                assert m.description == "integration test histogram", (
                    f"test.custom_histogram description mismatch: {m.description!r}"
                )
                return
        pytest.fail("test.custom_histogram not found")

    def test_custom_gauge_description(self, otel_collector: OtelCollector) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_gauge":
                assert m.description == "integration test gauge", (
                    f"test.custom_gauge description mismatch: {m.description!r}"
                )
                return
        pytest.fail("test.custom_gauge not found")

    # ── Data points have values ───────────────────────────────────────────

    def test_http_duration_has_data_points(self, otel_collector: OtelCollector) -> None:
        """The histogram should have at least one recorded observation."""
        for m in _flat_metrics(otel_collector):
            if m.name == "http.server.request.duration" and m.histogram:
                assert m.histogram.dataPoints, (
                    "http.server.request.duration histogram has no data points"
                )
                return
        pytest.fail("http.server.request.duration histogram not found")

    def test_custom_counter_has_data_points(
        self, otel_collector: OtelCollector
    ) -> None:
        for m in _flat_metrics(otel_collector):
            if m.name == "test.custom_counter" and m.sum:
                assert m.sum.dataPoints, "test.custom_counter sum has no data points"
                return
        pytest.fail("test.custom_counter sum not found")


# ---------------------------------------------------------------------------
# Tests — span attributes and structure
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestSpanAttributes:
    """Verify HTTP and custom spans carry correct attributes and structure."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        _generate_telemetry(telemetry_client)
        time.sleep(3)
        _wait_for_collector_data(otel_collector)

    # ── HTTP span structure ───────────────────────────────────────────────

    def test_http_span_exists(self, otel_collector: OtelCollector) -> None:
        spans = _flat_spans(otel_collector)
        http_spans = [s for s in spans if s.name == "http.server.request"]
        assert http_spans, (
            f"expected 'http.server.request' span; got {sorted(set(s.name for s in spans))}"
        )

    def test_http_span_kind_is_server(self, otel_collector: OtelCollector) -> None:
        """HTTP spans should have kind=SERVER (2)."""
        for s in _flat_spans(otel_collector):
            if s.name == "http.server.request":
                assert s.kind == OTEL_SPAN_KIND_SERVER, (
                    f"http.server.request kind should be SERVER (2), got {s.kind}"
                )
                return
        pytest.fail("http.server.request span not found")

    def test_http_span_has_method_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        for s in _flat_spans(otel_collector):
            if s.name == "http.server.request":
                attr_keys = {a.key for a in s.attributes}
                assert "http.request.method" in attr_keys, (
                    f"http.server.request missing 'http.request.method'; "
                    f"got {attr_keys}"
                )
                return
        pytest.fail("http.server.request span not found")

    def test_http_span_has_status_code_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        for s in _flat_spans(otel_collector):
            if s.name == "http.server.request":
                attr_keys = {a.key for a in s.attributes}
                assert "http.response.status_code" in attr_keys, (
                    f"http.server.request missing 'http.response.status_code'; "
                    f"got {attr_keys}"
                )
                return
        pytest.fail("http.server.request span not found")

    def test_http_span_has_url_path(self, otel_collector: OtelCollector) -> None:
        for s in _flat_spans(otel_collector):
            if s.name == "http.server.request":
                attr_keys = {a.key for a in s.attributes}
                assert "url.path" in attr_keys, (
                    f"http.server.request missing 'url.path'; got {attr_keys}"
                )
                return
        pytest.fail("http.server.request span not found")

    def test_http_span_has_valid_timestamps(
        self, otel_collector: OtelCollector
    ) -> None:
        for s in _flat_spans(otel_collector):
            if s.name == "http.server.request":
                assert s.startTimeUnixNano and s.startTimeUnixNano != "0", (
                    "http.server.request missing startTimeUnixNano"
                )
                assert s.endTimeUnixNano and s.endTimeUnixNano != "0", (
                    "http.server.request missing endTimeUnixNano"
                )
                return
        pytest.fail("http.server.request span not found")

    # ── Custom span structure ─────────────────────────────────────────────

    def test_custom_span_exists(self, otel_collector: OtelCollector) -> None:
        spans = _flat_spans(otel_collector)
        custom = [s for s in spans if s.name == "test.custom_span"]
        assert custom, (
            f"expected 'test.custom_span'; got {sorted(set(s.name for s in spans))}"
        )

    def test_custom_span_has_user_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        """The telemetry test endpoint passes surface='span' to the custom span."""
        for s in _flat_spans(otel_collector):
            if s.name == "test.custom_span":
                attrs = {a.key: a.value.stringValue for a in s.attributes}
                assert attrs.get("surface") == "span", (
                    f"test.custom_span missing surface='span'; got {attrs}"
                )
                return
        pytest.fail("test.custom_span not found")

    def test_custom_span_has_identity_attributes(
        self, otel_collector: OtelCollector
    ) -> None:
        """Python spans should carry apx.process.type identity attribute."""
        for s in _flat_spans(otel_collector):
            if s.name == "test.custom_span":
                attr_keys = {a.key for a in s.attributes}
                assert "apx.process.type" in attr_keys, (
                    f"test.custom_span missing 'apx.process.type'; got {attr_keys}"
                )
                assert "apx.worker.id" in attr_keys, (
                    f"test.custom_span missing 'apx.worker.id'; got {attr_keys}"
                )
                return
        pytest.fail("test.custom_span not found")

    # ── Log-level span structure ──────────────────────────────────────────

    def test_log_span_has_level_attribute(self, otel_collector: OtelCollector) -> None:
        """log.info() creates an instant span with log.level='info'."""
        for s in _flat_spans(otel_collector):
            if s.name == "integration test log message":
                attrs = {a.key: a.value.stringValue for a in s.attributes}
                assert attrs.get("log.level") == "info", (
                    f"log span missing log.level='info'; got {attrs}"
                )
                return
        pytest.fail("log span 'integration test log message' not found")

    def test_log_span_is_zero_duration(self, otel_collector: OtelCollector) -> None:
        """Instant (log-level) spans should have near-zero duration (< 1ms)."""
        for s in _flat_spans(otel_collector):
            if s.name == "integration test log message":
                start = int(s.startTimeUnixNano)
                end = int(s.endTimeUnixNano)
                delta_us = (end - start) / 1_000
                assert delta_us < 1_000, (
                    f"log span should be near-zero-duration; "
                    f"delta={delta_us:.1f}µs"
                )
                return
        pytest.fail("log span 'integration test log message' not found")
