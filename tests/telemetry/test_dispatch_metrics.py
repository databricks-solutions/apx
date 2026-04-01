"""Verify APX request pipeline metrics are collected when APX_PERF=1.

The telemetry_container fixture sets ``APX_PERF=1``, which enables all
``ApxMetrics`` toggles via ``_apx_perf_enabled()``. After sending HTTP
requests that exercise the full request pipeline, histogram and gauge
metrics must appear in the OTEL collector output.
"""

from __future__ import annotations

import time

import httpx
import pytest

from .conftest import (
    OtelCollector,
    flat_metrics_with_scope,
    wait_for_collector_data,
)

APX_HISTOGRAM_METRICS = {
    "apx.parse",
    "apx.scope_build",
    "apx.receive_build",
    "apx.send_parse",
    "apx.response_build",
    "apx.response_write",
    "apx.handler_wait",
    "apx.request_total",
}

APX_GAUGE_METRICS = {
    "apx.active_requests",
    "apx.connections",
}

APX_ALL_METRICS = APX_HISTOGRAM_METRICS | APX_GAUGE_METRICS


@pytest.mark.integration
class TestDispatchMetrics:
    """APX request pipeline metrics must appear when APX_PERF is enabled."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        for _ in range(10):
            telemetry_client.get("/api/health")
            telemetry_client.post("/api/upload", content=b'{"ping": true}')
        time.sleep(5)
        wait_for_collector_data(otel_collector)

    def test_all_dispatch_metrics_present(self, otel_collector: OtelCollector) -> None:
        """Every APX metric must have at least one data point."""
        collected_names = {m.name for _, m in flat_metrics_with_scope(otel_collector)}
        missing = APX_ALL_METRICS - collected_names
        assert not missing, (
            f"Missing APX metrics: {sorted(missing)}. "
            f"Collected metric names: {sorted(collected_names)}"
        )

    def test_histogram_metrics_are_histograms(
        self, otel_collector: OtelCollector
    ) -> None:
        """APX histogram metrics must be exported as histograms."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_HISTOGRAM_METRICS:
                assert m.histogram is not None, (
                    f"{m.name} should be a histogram, got sum={m.sum} gauge={m.gauge}"
                )

    def test_histogram_metrics_unit_is_microseconds(
        self, otel_collector: OtelCollector
    ) -> None:
        """APX histogram metrics must report in microseconds."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_HISTOGRAM_METRICS:
                assert m.unit == "us", f"{m.name} unit should be 'us', got {m.unit!r}"

    def test_gauge_metrics_unit_is_dimensionless(
        self, otel_collector: OtelCollector
    ) -> None:
        """Gauge metrics (active_requests, connections) use dimensionless unit."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_GAUGE_METRICS:
                assert m.unit == "1", f"{m.name} unit should be '1', got {m.unit!r}"
