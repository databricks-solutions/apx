"""Verify nested span parent-child relationships via OTLP export.

Hits ``/api/telemetry/nested-spans`` which creates a 3-level nesting::

    test.outer (depth=1)
      └─ test.middle (depth=2)
           └─ test.inner (depth=3)

All three spans should share the same ``traceId``, and each child's
``parentSpanId`` must point to its immediate parent's ``spanId``.
"""

from __future__ import annotations

import time

import httpx
import pytest

from .conftest import (
    OtelCollector,
    flat_spans,
    span_attrs,
    wait_for_collector_data,
)


@pytest.mark.integration
class TestNestedSpans:
    """Verify 3-level nested span parent-child chain."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        r = telemetry_client.get("/api/telemetry/nested-spans")
        assert r.status_code == 200
        time.sleep(3)
        wait_for_collector_data(otel_collector)

    def _find(self, collector: OtelCollector, name: str):
        for s in flat_spans(collector):
            if s.name == name:
                return s
        all_names = sorted({s.name for s in flat_spans(collector)})
        pytest.fail(f"span {name!r} not found; available: {all_names}")

    def test_outer_span_exists(self, otel_collector: OtelCollector) -> None:
        span = self._find(otel_collector, "test.outer")
        assert span_attrs(span).get("depth") == "1"

    def test_middle_span_exists(self, otel_collector: OtelCollector) -> None:
        span = self._find(otel_collector, "test.middle")
        assert span_attrs(span).get("depth") == "2"

    def test_inner_span_exists(self, otel_collector: OtelCollector) -> None:
        span = self._find(otel_collector, "test.inner")
        assert span_attrs(span).get("depth") == "3"

    def test_all_share_same_trace_id(self, otel_collector: OtelCollector) -> None:
        outer = self._find(otel_collector, "test.outer")
        middle = self._find(otel_collector, "test.middle")
        inner = self._find(otel_collector, "test.inner")

        assert outer.traceId == middle.traceId, (
            f"outer and middle traceId mismatch: {outer.traceId} != {middle.traceId}"
        )
        assert middle.traceId == inner.traceId, (
            f"middle and inner traceId mismatch: {middle.traceId} != {inner.traceId}"
        )

    def test_inner_parent_is_middle(self, otel_collector: OtelCollector) -> None:
        middle = self._find(otel_collector, "test.middle")
        inner = self._find(otel_collector, "test.inner")

        assert inner.parentSpanId == middle.spanId, (
            f"inner.parentSpanId ({inner.parentSpanId}) "
            f"should equal middle.spanId ({middle.spanId})"
        )

    def test_middle_parent_is_outer(self, otel_collector: OtelCollector) -> None:
        outer = self._find(otel_collector, "test.outer")
        middle = self._find(otel_collector, "test.middle")

        assert middle.parentSpanId == outer.spanId, (
            f"middle.parentSpanId ({middle.parentSpanId}) "
            f"should equal outer.spanId ({outer.spanId})"
        )

    def test_outer_is_child_of_http_span(self, otel_collector: OtelCollector) -> None:
        """The outer user span should be a child of the HTTP root span."""
        outer = self._find(otel_collector, "test.outer")
        assert outer.parentSpanId, "outer span should have a parentSpanId (HTTP root)"

        http_span = None
        for s in flat_spans(otel_collector):
            if s.name == "http.server.request" and s.traceId == outer.traceId:
                http_span = s
                break

        assert http_span is not None, (
            "expected http.server.request span with same traceId as outer"
        )

    def test_all_span_ids_are_distinct(self, otel_collector: OtelCollector) -> None:
        outer = self._find(otel_collector, "test.outer")
        middle = self._find(otel_collector, "test.middle")
        inner = self._find(otel_collector, "test.inner")

        ids = {outer.spanId, middle.spanId, inner.spanId}
        assert len(ids) == 3, f"expected 3 distinct spanIds; got {ids}"
