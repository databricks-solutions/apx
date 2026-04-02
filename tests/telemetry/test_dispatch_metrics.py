"""Verify APX request pipeline metrics are collected when APX_PERF=1.

The telemetry_container fixture sets ``APX_PERF=1``, which enables all
``ApxMetrics`` toggles via ``_apx_perf_enabled()``. After sending HTTP
requests that exercise the full request pipeline, histogram and
up-down counter metrics must appear in the OTEL collector output.
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

APX_UP_DOWN_COUNTER_METRICS = {
    "apx.active_requests",
    "apx.connections",
}

APX_ALL_METRICS = APX_HISTOGRAM_METRICS | APX_UP_DOWN_COUNTER_METRICS


def _histogram_count(otel_collector: OtelCollector, name: str) -> int:
    """Sum observation counts across all exported histogram data points."""
    total = 0
    for _, m in flat_metrics_with_scope(otel_collector):
        if m.name == name and m.histogram is not None:
            for dp in m.histogram.dataPoints:
                if dp.count is not None:
                    total += int(dp.count)
    return total


def _histogram_sum(otel_collector: OtelCollector, name: str) -> float:
    """Sum all histogram sums across exported data points."""
    total = 0.0
    for _, m in flat_metrics_with_scope(otel_collector):
        if m.name == name and m.histogram is not None:
            for dp in m.histogram.dataPoints:
                if dp.sum is not None:
                    total += dp.sum
    return total


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

    def test_up_down_counter_metrics_are_sum_type(
        self, otel_collector: OtelCollector
    ) -> None:
        """active_requests and connections must be OTLP sum (UpDownCounter), not gauge."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_UP_DOWN_COUNTER_METRICS:
                assert m.sum is not None, (
                    f"{m.name} should be sum (UpDownCounter), "
                    f"got gauge={m.gauge} histogram={m.histogram}"
                )
                assert m.gauge is None, (
                    f"{m.name} must not be a gauge (was migrated to UpDownCounter)"
                )

    def test_up_down_counter_metrics_unit_is_dimensionless(
        self, otel_collector: OtelCollector
    ) -> None:
        """Up-down counter metrics (active_requests, connections) use dimensionless unit."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name in APX_UP_DOWN_COUNTER_METRICS:
                assert m.unit == "1", f"{m.name} unit should be '1', got {m.unit!r}"

    def test_send_parse_count_gte_request_total(
        self, otel_collector: OtelCollector
    ) -> None:
        """send_parse fires per ASGI send event (>= 2 per request), so its count must exceed request_total."""
        send_parse_count = _histogram_count(otel_collector, "apx.send_parse")
        request_total_count = _histogram_count(otel_collector, "apx.request_total")
        assert send_parse_count >= request_total_count, (
            f"send_parse count ({send_parse_count}) should be >= "
            f"request_total count ({request_total_count}): "
            f"each request has at least 2 send events (start + body)"
        )

    def test_histogram_boundaries_have_sub_millisecond_resolution(
        self, otel_collector: OtelCollector
    ) -> None:
        """µs histograms must have boundaries below 1000µs for sub-ms latency phases."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name == "apx.parse" and m.histogram is not None:
                for dp in m.histogram.dataPoints:
                    if not dp.explicitBounds:
                        continue
                    sub_ms = [b for b in dp.explicitBounds if b < 1000.0]
                    assert len(sub_ms) >= 6, (
                        f"expected ≥6 boundaries below 1000µs, got {sub_ms}"
                    )
                    return
        pytest.skip("apx.parse histogram with explicitBounds not found")

    def test_request_total_measures_full_lifecycle(
        self, otel_collector: OtelCollector
    ) -> None:
        """request_total and handler_wait should measure the same interval."""
        rt_count = _histogram_count(otel_collector, "apx.request_total")
        hw_count = _histogram_count(otel_collector, "apx.handler_wait")
        if rt_count == 0 or hw_count == 0:
            pytest.skip("no request_total or handler_wait observations")

        rt_mean = _histogram_sum(otel_collector, "apx.request_total") / rt_count
        hw_mean = _histogram_sum(otel_collector, "apx.handler_wait") / hw_count

        ratio = rt_mean / hw_mean if hw_mean > 0 else float("inf")
        assert 0.5 <= ratio <= 2.0, (
            f"request_total mean ({rt_mean:.0f}µs) and handler_wait mean "
            f"({hw_mean:.0f}µs) should be in the same order of magnitude "
            f"(ratio={ratio:.2f}), since both measure dispatch-to-response-complete"
        )
