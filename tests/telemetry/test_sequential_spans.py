"""Verify sequential sibling spans share the same parent.

Hits ``/api/telemetry/sequential-spans`` which creates::

    test.parent (role=parent)
      ├─ test.sibling_a (order=first)   [enters, exits]
      └─ test.sibling_b (order=second)  [enters, exits]

Both siblings should have ``parentSpanId == parent.spanId``, distinct
``spanId`` values, and the same ``traceId``.  ``sibling_a`` should
finish before ``sibling_b`` starts.
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
class TestSequentialSpans:
    """Verify sibling span relationships and ordering."""

    @pytest.fixture(autouse=True, scope="class")
    def _setup(
        self,
        telemetry_client: httpx.Client,
        otel_collector: OtelCollector,
    ) -> None:
        r = telemetry_client.get("/api/telemetry/sequential-spans")
        assert r.status_code == 200
        time.sleep(3)
        wait_for_collector_data(otel_collector)

    def _find(self, collector: OtelCollector, name: str):
        for s in flat_spans(collector):
            if s.name == name:
                return s
        all_names = sorted({s.name for s in flat_spans(collector)})
        pytest.fail(f"span {name!r} not found; available: {all_names}")

    def test_parent_span_exists(self, otel_collector: OtelCollector) -> None:
        parent = self._find(otel_collector, "test.parent")
        assert span_attrs(parent).get("role") == "parent"

    def test_sibling_a_exists(self, otel_collector: OtelCollector) -> None:
        a = self._find(otel_collector, "test.sibling_a")
        assert span_attrs(a).get("order") == "first"

    def test_sibling_b_exists(self, otel_collector: OtelCollector) -> None:
        b = self._find(otel_collector, "test.sibling_b")
        assert span_attrs(b).get("order") == "second"

    def test_all_share_same_trace_id(self, otel_collector: OtelCollector) -> None:
        parent = self._find(otel_collector, "test.parent")
        a = self._find(otel_collector, "test.sibling_a")
        b = self._find(otel_collector, "test.sibling_b")

        assert parent.traceId == a.traceId == b.traceId, (
            f"traceId mismatch: parent={parent.traceId} a={a.traceId} b={b.traceId}"
        )

    def test_sibling_a_parent_is_parent_span(
        self, otel_collector: OtelCollector
    ) -> None:
        parent = self._find(otel_collector, "test.parent")
        a = self._find(otel_collector, "test.sibling_a")

        assert a.parentSpanId == parent.spanId, (
            f"sibling_a.parentSpanId ({a.parentSpanId}) "
            f"should equal parent.spanId ({parent.spanId})"
        )

    def test_sibling_b_parent_is_parent_span(
        self, otel_collector: OtelCollector
    ) -> None:
        parent = self._find(otel_collector, "test.parent")
        b = self._find(otel_collector, "test.sibling_b")

        assert b.parentSpanId == parent.spanId, (
            f"sibling_b.parentSpanId ({b.parentSpanId}) "
            f"should equal parent.spanId ({parent.spanId})"
        )

    def test_siblings_have_different_span_ids(
        self, otel_collector: OtelCollector
    ) -> None:
        a = self._find(otel_collector, "test.sibling_a")
        b = self._find(otel_collector, "test.sibling_b")

        assert a.spanId != b.spanId, (
            f"siblings should have distinct spanIds; both are {a.spanId}"
        )

    def test_sibling_a_finishes_before_b_starts(
        self, otel_collector: OtelCollector
    ) -> None:
        """Sequential ordering: sibling_a.endTime <= sibling_b.startTime."""
        a = self._find(otel_collector, "test.sibling_a")
        b = self._find(otel_collector, "test.sibling_b")

        a_end = int(a.endTimeUnixNano)
        b_start = int(b.startTimeUnixNano)

        assert a_end <= b_start, (
            f"sibling_a should finish before sibling_b starts: "
            f"a.end={a_end} b.start={b_start}"
        )

    def test_parent_encloses_both_siblings(
        self, otel_collector: OtelCollector
    ) -> None:
        """Parent span should start before and end after both children."""
        parent = self._find(otel_collector, "test.parent")
        a = self._find(otel_collector, "test.sibling_a")
        b = self._find(otel_collector, "test.sibling_b")

        p_start = int(parent.startTimeUnixNano)
        p_end = int(parent.endTimeUnixNano)
        a_start = int(a.startTimeUnixNano)
        b_end = int(b.endTimeUnixNano)

        assert p_start <= a_start, (
            f"parent should start before sibling_a: {p_start} > {a_start}"
        )
        assert p_end >= b_end, (
            f"parent should end after sibling_b: {p_end} < {b_end}"
        )
