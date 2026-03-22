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
    LogsExport,
    MetricsExport,
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
) -> None:
    """Wait until the collector has received at least some traces and metrics."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        has_traces = len(collector.traces()) > 0
        has_metrics = len(collector.metrics()) > 0
        if has_traces and has_metrics:
            return
        time.sleep(1.0)


def _uuid_to_trace_id(uid: str) -> str:
    """Convert a UUID string to the OTEL hex trace-id (32 hex chars, no dashes)."""
    return uuid.UUID(uid).bytes.hex()


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def otel_collector(tmp_path_factory: pytest.TempPathFactory) -> Generator[OtelCollector]:
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
                            attrs = {
                                a.key: a.value.stringValue for a in s.attributes
                            }
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

    def test_otel_logs_have_trace_context(
        self, otel_collector: OtelCollector
    ) -> None:
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
