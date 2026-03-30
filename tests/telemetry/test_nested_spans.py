"""Verify nested span parent-child relationships via OTLP export.

Hits ``/api/telemetry/nested-spans`` which creates a 3-level nesting::

    test.outer (depth=1)
      └─ test.middle (depth=2)
           └─ test.inner (depth=3)

All three spans should share the same ``traceId``, and each child's
``parentSpanId`` must point to its immediate parent's ``spanId``.
"""

from __future__ import annotations

import pytest

from .conftest import (
    OtelCollector,
    find_span,
    flat_spans,
    make_setup_fixture,
    span_attrs,
)


@pytest.mark.integration
class TestNestedSpans:
    """Verify 3-level nested span parent-child chain."""

    _setup = make_setup_fixture("/api/telemetry/nested-spans", sleep_time=3)

    def test_outer_span_exists(self, otel_collector: OtelCollector) -> None:
        span = find_span(otel_collector, "test.outer")
        assert span_attrs(span).get("depth") == "1"

    def test_middle_span_exists(self, otel_collector: OtelCollector) -> None:
        span = find_span(otel_collector, "test.middle")
        assert span_attrs(span).get("depth") == "2"

    def test_inner_span_exists(self, otel_collector: OtelCollector) -> None:
        span = find_span(otel_collector, "test.inner")
        assert span_attrs(span).get("depth") == "3"

    def test_all_share_same_trace_id(self, otel_collector: OtelCollector) -> None:
        outer = find_span(otel_collector, "test.outer")
        middle = find_span(otel_collector, "test.middle")
        inner = find_span(otel_collector, "test.inner")

        assert outer.traceId == middle.traceId, (
            f"outer and middle traceId mismatch: {outer.traceId} != {middle.traceId}"
        )
        assert middle.traceId == inner.traceId, (
            f"middle and inner traceId mismatch: {middle.traceId} != {inner.traceId}"
        )

    def test_inner_parent_is_middle(self, otel_collector: OtelCollector) -> None:
        middle = find_span(otel_collector, "test.middle")
        inner = find_span(otel_collector, "test.inner")

        assert inner.parentSpanId == middle.spanId, (
            f"inner.parentSpanId ({inner.parentSpanId}) "
            f"should equal middle.spanId ({middle.spanId})"
        )

    def test_middle_parent_is_outer(self, otel_collector: OtelCollector) -> None:
        outer = find_span(otel_collector, "test.outer")
        middle = find_span(otel_collector, "test.middle")

        assert middle.parentSpanId == outer.spanId, (
            f"middle.parentSpanId ({middle.parentSpanId}) "
            f"should equal outer.spanId ({outer.spanId})"
        )

    def test_outer_is_child_of_http_span(self, otel_collector: OtelCollector) -> None:
        """The outer user span should be a child of the HTTP root span."""
        outer = find_span(otel_collector, "test.outer")
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
        outer = find_span(otel_collector, "test.outer")
        middle = find_span(otel_collector, "test.middle")
        inner = find_span(otel_collector, "test.inner")

        ids = {outer.spanId, middle.spanId, inner.spanId}
        assert len(ids) == 3, f"expected 3 distinct spanIds; got {ids}"
