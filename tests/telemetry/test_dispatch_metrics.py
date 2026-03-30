"""Verify all 9 APX dispatch pipeline metrics are collected when APX_PERF=1.

The telemetry_container fixture sets ``APX_PERF=1``, which enables all
``ApxMetrics`` toggles via ``_apx_perf_enabled()``. After sending HTTP
requests that exercise the full dispatch pipeline (receive + send), all
9 histogram metrics must appear in the OTEL collector output.
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

APX_DISPATCH_METRICS = {
    "apx.dispatch.body_collect.duration",
    "apx.dispatch.crossbeam_send.duration",
    "apx.dispatch.response_wait.duration",
    "apx.dispatch.total.duration",
    "apx.asgi.receive_build.duration",
    "apx.asgi.send_parse.duration",
    "apx.dispatch.pickup_delay.duration",
    "apx.dispatch.materialize.duration",
    "apx.dispatch.queue_depth",
}


@pytest.mark.integration
class TestDispatchMetrics:
    """All APX dispatch histograms must appear when APX_PERF is enabled."""

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

    def test_all_dispatch_metrics_present(
        self, otel_collector: OtelCollector
    ) -> None:
        """Every APX dispatch histogram must have at least one data point."""
        collected_names = {
            m.name for _, m in flat_metrics_with_scope(otel_collector)
        }
        missing = APX_DISPATCH_METRICS - collected_names
        assert not missing, (
            f"Missing APX dispatch metrics: {sorted(missing)}. "
            f"Collected metric names: {sorted(collected_names)}"
        )

    def test_dispatch_metrics_are_histograms(
        self, otel_collector: OtelCollector
    ) -> None:
        """APX dispatch metrics must be exported as histograms."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_DISPATCH_METRICS:
                assert m.histogram is not None, (
                    f"{m.name} should be a histogram, got sum={m.sum} gauge={m.gauge}"
                )

    def test_dispatch_metrics_unit_is_microseconds(
        self, otel_collector: OtelCollector
    ) -> None:
        """APX dispatch duration metrics must report in microseconds."""
        duration_metrics = {n for n in APX_DISPATCH_METRICS if n.endswith(".duration")}
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in duration_metrics:
                assert m.unit == "us", (
                    f"{m.name} unit should be 'us', got {m.unit!r}"
                )

    def test_queue_depth_unit_is_dimensionless(
        self, otel_collector: OtelCollector
    ) -> None:
        """queue_depth is a count, not a duration — unit must be '1'."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name == "apx.dispatch.queue_depth":
                assert m.unit == "1", (
                    f"{m.name} unit should be '1', got {m.unit!r}"
                )
