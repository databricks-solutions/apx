"""Integration tests for OTEL telemetry exported via gRPC.

Spins up a minimal gRPC OTLP receiver, starts the APX server in a Docker
container with OTEL_EXPORTER_OTLP_ENDPOINT pointing at the receiver, makes
HTTP requests, and verifies traces/metrics/logs arrive.
"""

from __future__ import annotations

import platform
import socket
import threading
import time
from concurrent import futures
from typing import Generator, Literal

import docker
import docker.errors
import docker.models.containers
import grpc
import httpx
import pytest
from opentelemetry.proto.collector.logs.v1 import (
    logs_service_pb2,
    logs_service_pb2_grpc,
)
from opentelemetry.proto.collector.metrics.v1 import (
    metrics_service_pb2,
    metrics_service_pb2_grpc,
)
from opentelemetry.proto.collector.trace.v1 import (
    trace_service_pb2,
    trace_service_pb2_grpc,
)

CONTAINER_NAME = "apx-telemetry-test"


# ---------------------------------------------------------------------------
# Minimal OTLP gRPC receiver
# ---------------------------------------------------------------------------


class _TraceCollector(trace_service_pb2_grpc.TraceServiceServicer):
    def __init__(self) -> None:
        self.resource_spans: list = []
        self._lock = threading.Lock()

    def Export(self, request, context):  # noqa: N802
        with self._lock:
            self.resource_spans.extend(request.resource_spans)
        return trace_service_pb2.ExportTraceServiceResponse()


class _MetricCollector(metrics_service_pb2_grpc.MetricsServiceServicer):
    def __init__(self) -> None:
        self.resource_metrics: list = []
        self._lock = threading.Lock()

    def Export(self, request, context):  # noqa: N802
        with self._lock:
            self.resource_metrics.extend(request.resource_metrics)
        return metrics_service_pb2.ExportMetricsServiceResponse()


class _LogCollector(logs_service_pb2_grpc.LogsServiceServicer):
    def __init__(self) -> None:
        self.resource_logs: list = []
        self._lock = threading.Lock()

    def Export(self, request, context):  # noqa: N802
        with self._lock:
            self.resource_logs.extend(request.resource_logs)
        return logs_service_pb2.ExportLogsServiceResponse()


class OtlpCollector:
    """Aggregates received OTLP data from all three signal collectors."""

    def __init__(
        self,
        traces: _TraceCollector,
        metrics: _MetricCollector,
        logs: _LogCollector,
        server: grpc.Server,
        port: int,
    ) -> None:
        self.traces = traces
        self.metrics = metrics
        self.logs = logs
        self._server = server
        self.port = port

    def stop(self) -> None:
        self._server.stop(grace=5)


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def _start_otlp_collector() -> OtlpCollector:
    port = _find_free_port()
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=4))

    traces = _TraceCollector()
    metrics = _MetricCollector()
    logs = _LogCollector()

    trace_service_pb2_grpc.add_TraceServiceServicer_to_server(traces, server)
    metrics_service_pb2_grpc.add_MetricsServiceServicer_to_server(metrics, server)
    logs_service_pb2_grpc.add_LogsServiceServicer_to_server(logs, server)

    server.add_insecure_port(f"[::]:{port}")
    server.start()

    return OtlpCollector(traces, metrics, logs, server, port)


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


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def otlp_collector() -> Generator[OtlpCollector]:
    """Start a gRPC OTLP receiver on a free port."""
    collector = _start_otlp_collector()
    print(f"[otlp] gRPC collector listening on port {collector.port}")
    yield collector
    collector.stop()


@pytest.fixture(scope="module")
def telemetry_container(
    apx_image: str,
    otlp_collector: OtlpCollector,
) -> Generator[str]:
    """Start an APX container with OTEL env vars pointing at the collector."""
    dk = docker.from_env()

    # Remove stale container if present.
    try:
        stale = dk.containers.get(CONTAINER_NAME)
        stale.remove(force=True)
    except docker.errors.NotFound:
        pass

    is_linux = platform.system() == "Linux"

    extra_hosts: dict[str, str] = {}
    if is_linux:
        extra_hosts["host.docker.internal"] = "host-gateway"

    endpoint = f"http://host.docker.internal:{otlp_collector.port}"

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
# Helpers
# ---------------------------------------------------------------------------


def _generate_telemetry(client: httpx.Client) -> None:
    """Hit endpoints that produce all three telemetry signals."""
    r = client.get("/api/echo")
    assert r.status_code == 200

    r = client.get("/api/telemetry/test")
    assert r.status_code == 200
    assert r.json() == {"ok": True}


def _wait_for_telemetry(
    collector: OtlpCollector,
    *,
    timeout: float = 30,
) -> None:
    """Wait until the collector has received at least some traces and metrics."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        has_traces = len(collector.traces.resource_spans) > 0
        has_metrics = len(collector.metrics.resource_metrics) > 0
        if has_traces and has_metrics:
            return
        time.sleep(1.0)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.integration
class TestTelemetry:
    """Verify OTLP gRPC export of traces, metrics, and logs."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otlp_collector: OtlpCollector,
    ) -> None:
        """Generate telemetry once for all tests in this class."""
        _generate_telemetry(telemetry_client)
        _generate_telemetry(telemetry_client)
        _wait_for_telemetry(otlp_collector)

    def test_traces_collected(self, otlp_collector: OtlpCollector) -> None:
        """HTTP request spans and custom SpanHandle spans arrive."""
        all_span_names: set[str] = set()
        for rs in otlp_collector.traces.resource_spans:
            for ss in rs.scope_spans:
                for span in ss.spans:
                    all_span_names.add(span.name)

        assert "test.custom_span" in all_span_names, (
            f"expected 'test.custom_span' in exported spans; got {all_span_names}"
        )

    def test_metrics_collected(self, otlp_collector: OtlpCollector) -> None:
        """HTTP and custom metrics (counter, histogram, gauge) arrive."""
        all_metric_names: set[str] = set()
        for rm in otlp_collector.metrics.resource_metrics:
            for sm in rm.scope_metrics:
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

    def test_logs_collected(self, otlp_collector: OtlpCollector) -> None:
        """Python log messages forwarded via tracing arrive as OTLP logs."""
        all_log_bodies: list[str] = []
        for rl in otlp_collector.logs.resource_logs:
            for sl in rl.scope_logs:
                for lr in sl.log_records:
                    all_log_bodies.append(lr.body.string_value)

        assert any("integration test log message" in b for b in all_log_bodies), (
            f"expected log containing 'integration test log message'; got {all_log_bodies[:20]}"
        )

    def test_log_level_spans_collected(self, otlp_collector: OtlpCollector) -> None:
        """log.info() produces an instant span with log.level attribute."""
        for rs in otlp_collector.traces.resource_spans:
            for ss in rs.scope_spans:
                for s in ss.spans:
                    if s.name == "integration test log message":
                        attrs = {a.key: a.value.string_value for a in s.attributes}
                        assert attrs.get("log.level") == "info", (
                            f"expected log.level='info'; got {attrs}"
                        )
                        return

        all_span_names = {
            s.name
            for rs in otlp_collector.traces.resource_spans
            for ss in rs.scope_spans
            for s in ss.spans
        }
        pytest.fail(
            f"expected span 'integration test log message'; got {all_span_names}"
        )

    def test_resource_attributes(self, otlp_collector: OtlpCollector) -> None:
        """Resource carries service.name and custom attributes."""
        assert otlp_collector.traces.resource_spans, "no trace data received"

        resource = otlp_collector.traces.resource_spans[0].resource
        attrs = {a.key: a.value.string_value for a in resource.attributes}

        assert attrs.get("service.name") == "apx-integration-test", (
            f"expected service.name='apx-integration-test'; got {attrs}"
        )
        assert attrs.get("workspace.id") == "test-ws", (
            f"expected workspace.id='test-ws'; got {attrs}"
        )
        assert attrs.get("app.name") == "bench-apx", (
            f"expected app.name='bench-apx'; got {attrs}"
        )
