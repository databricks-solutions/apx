"""Telemetry test fixtures: OTEL Collector + APX container with OTLP export.

Provides module-scoped fixtures that start an OTEL Collector (writing JSONL)
and an APX container configured to export to it.  Shared helper functions
for flattening and querying exported OTLP data live here so individual test
modules stay focused on assertions.
"""

from __future__ import annotations

import platform
import socket
import sys
import textwrap
import time
from pathlib import Path
from typing import Generator, Literal

import docker
import docker.errors
import docker.models.containers
import httpx
import pytest

# otlp_models lives in tests/integration/ — add it to sys.path so both
# test directories can share the Pydantic OTLP models.
_INTEGRATION_DIR = str(Path(__file__).resolve().parent.parent / "integration")
if _INTEGRATION_DIR not in sys.path:
    sys.path.insert(0, _INTEGRATION_DIR)

from otlp_models import (  # noqa: E402
    InstrumentationScope,
    LogRecord,
    LogsExport,
    Metric,
    MetricsExport,
    ResourceLogs,
    ResourceMetrics,
    ResourceSpans,
    Span,
    TracesExport,
    read_jsonl,
)

CONTAINER_NAME = "apx-telemetry-suite-test"
COLLECTOR_CONTAINER_NAME = "apx-otel-collector-suite-test"
COLLECTOR_IMAGE = "otel/opentelemetry-collector:0.120.0"


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


def wait_for_collector_data(
    collector: OtelCollector,
    *,
    timeout: float = 30,
    require_logs: bool = False,
) -> None:
    """Wait until the collector has received at least some traces and metrics."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        has_traces = len(collector.traces()) > 0
        has_metrics = len(collector.metrics()) > 0
        has_logs = not require_logs or len(collector.logs()) > 0
        if has_traces and has_metrics and has_logs:
            return
        time.sleep(1.0)


# ---------------------------------------------------------------------------
# Span / log / metric flattening helpers
# ---------------------------------------------------------------------------


def flat_spans(collector: OtelCollector) -> list[Span]:
    """Flatten all Span objects from the collector."""
    spans: list[Span] = []
    for export in collector.traces():
        for rs in export.resourceSpans:
            for ss in rs.scopeSpans:
                spans.extend(ss.spans)
    return spans


def flat_spans_with_scope(
    collector: OtelCollector,
) -> list[tuple[InstrumentationScope, Span]]:
    """Flatten all (scope, span) pairs from the collector."""
    result: list[tuple[InstrumentationScope, Span]] = []
    for export in collector.traces():
        for rs in export.resourceSpans:
            for ss in rs.scopeSpans:
                for span in ss.spans:
                    result.append((ss.scope, span))
    return result


def flat_log_records(
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


def flat_metrics_with_scope(
    collector: OtelCollector,
) -> list[tuple[InstrumentationScope, Metric]]:
    """Flatten all (scope, metric) pairs from the collector."""
    result: list[tuple[InstrumentationScope, Metric]] = []
    for export in collector.metrics():
        for rm in export.resourceMetrics:
            for sm in rm.scopeMetrics:
                for m in sm.metrics:
                    result.append((sm.scope, m))
    return result


def flat_resource_spans(collector: OtelCollector) -> list[ResourceSpans]:
    """All ResourceSpans envelopes (with resource + schemaUrl)."""
    result: list[ResourceSpans] = []
    for export in collector.traces():
        result.extend(export.resourceSpans)
    return result


def flat_resource_logs(collector: OtelCollector) -> list[ResourceLogs]:
    """All ResourceLogs envelopes (with resource + schemaUrl)."""
    result: list[ResourceLogs] = []
    for export in collector.logs():
        result.extend(export.resourceLogs)
    return result


def flat_resource_metrics(collector: OtelCollector) -> list[ResourceMetrics]:
    """All ResourceMetrics envelopes (with resource + schemaUrl)."""
    result: list[ResourceMetrics] = []
    for export in collector.metrics():
        result.extend(export.resourceMetrics)
    return result


def span_attrs(span: Span) -> dict[str, str]:
    """Extract span attributes as a {key: stringValue} dict."""
    return {a.key: (a.value.stringValue or "") for a in span.attributes}


def log_attrs(lr: LogRecord) -> dict[str, str]:
    """Extract log record attributes as a {key: stringValue} dict."""
    return {a.key: (a.value.stringValue or "") for a in lr.attributes}


def find_span(collector: OtelCollector, name: str) -> Span:
    """Find a span by name, or fail with a list of available span names."""
    for s in flat_spans(collector):
        if s.name == name:
            return s
    all_names = sorted({s.name for s in flat_spans(collector)})
    pytest.fail(f"span {name!r} not found; available: {all_names}")


def make_setup_fixture(
    endpoint: str,
    sleep_time: float = 3,
    require_logs: bool = False,
):
    """Factory for the common class-scoped ``_setup`` fixture pattern.

    Returns a pytest fixture that: GETs ``endpoint``, sleeps, and waits
    for the OTEL collector to receive data.
    """

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        r = telemetry_client.get(endpoint)
        assert r.status_code == 200
        time.sleep(sleep_time)
        wait_for_collector_data(otel_collector, require_logs=require_logs)

    return _setup


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
            "OTEL_EXPORTER_OTLP_ENDPOINT": endpoint,
            "OTEL_EXPORTER_OTLP_PROTOCOL": "grpc",
            "OTEL_SERVICE_NAME": "apx-telemetry-suite",
            "OTEL_RESOURCE_ATTRIBUTES": "workspace.id=test-ws,app.name=bench-apx",
            "OTEL_BSP_SCHEDULE_DELAY": "500",
            "OTEL_BLRP_SCHEDULE_DELAY": "500",
            "OTEL_BSP_MAX_EXPORT_BATCH_SIZE": "16",
            "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE": "16",
            "APX_PERF": "1",
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
