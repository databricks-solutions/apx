"""Verify cross-signal telemetry: instrumentation scopes and correlation.

Hits ``/api/telemetry/cross-signal`` which exercises within a single request:
- User span (``test.cross_signal_span``)
- ``log.info()`` and ``log.warn()`` (instant spans with ``log.level``)
- ``Counter.inc()`` and ``Histogram.observe()`` (user metrics)
- ``logging.getLogger().warning()`` (stdlib log → OTLP log record)

Tests verify:
1. Instrumentation scope names are correct for each signal type.
2. Log-level spans carry the correct ``log.level`` attribute.
3. Stdlib Python logs appear as OTLP log records (not spans).
4. User metrics are attributed to the correct scope.
5. All user spans share the HTTP request's ``traceId``.
"""

from __future__ import annotations

import pytest

from .conftest import (
    OtelCollector,
    flat_log_records,
    flat_metrics_with_scope,
    flat_spans,
    flat_spans_with_scope,
    make_setup_fixture,
    span_attrs,
)


@pytest.mark.integration
class TestCrossSignal:
    """Verify instrumentation scope names and cross-signal correlation."""

    _setup = make_setup_fixture(
        "/api/telemetry/cross-signal", sleep_time=5, require_logs=True,
    )

    # ── Instrumentation scope: user spans ─────────────────────────────────

    def test_user_span_scope_is_apx_user(
        self, otel_collector: OtelCollector
    ) -> None:
        """User spans (via apx.telemetry.span) should have scope 'apx.user'."""
        for scope, span in flat_spans_with_scope(otel_collector):
            if span.name == "test.cross_signal_span":
                assert scope.name == "apx.user", (
                    f"expected scope 'apx.user' for user span; got {scope.name!r}"
                )
                return
        pytest.fail("test.cross_signal_span not found in exported spans")

    def test_user_span_has_surface_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        for span in flat_spans(otel_collector):
            if span.name == "test.cross_signal_span":
                attrs = span_attrs(span)
                assert attrs.get("surface") == "cross"
                return
        pytest.fail("test.cross_signal_span not found")

    # ── Instrumentation scope: log-level spans ────────────────────────────

    def test_log_info_span_scope_is_apx_user(
        self, otel_collector: OtelCollector
    ) -> None:
        """log.info() instant spans should also use scope 'apx.user'."""
        for scope, span in flat_spans_with_scope(otel_collector):
            if span.name == "cross signal info log":
                assert scope.name == "apx.user", (
                    f"expected scope 'apx.user' for log span; got {scope.name!r}"
                )
                return
        pytest.fail("'cross signal info log' span not found")

    def test_log_info_span_has_level_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        for span in flat_spans(otel_collector):
            if span.name == "cross signal info log":
                attrs = span_attrs(span)
                assert attrs.get("log.level") == "info", (
                    f"expected log.level='info'; got {attrs}"
                )
                return
        pytest.fail("'cross signal info log' span not found")

    def test_log_warn_span_has_level_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        for span in flat_spans(otel_collector):
            if span.name == "cross signal warn log":
                attrs = span_attrs(span)
                assert attrs.get("log.level") == "warn", (
                    f"expected log.level='warn'; got {attrs}"
                )
                return
        pytest.fail("'cross signal warn log' span not found")

    def test_log_warn_span_scope_is_apx_user(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, span in flat_spans_with_scope(otel_collector):
            if span.name == "cross signal warn log":
                assert scope.name == "apx.user", (
                    f"expected scope 'apx.user' for warn log span; got {scope.name!r}"
                )
                return
        pytest.fail("'cross signal warn log' span not found")

    # ── Instrumentation scope: stdlib log records ─────────────────────────

    def test_stdlib_log_appears_as_otel_log_record(
        self, otel_collector: OtelCollector
    ) -> None:
        """Python stdlib logging.warning() should appear as an OTLP log record."""
        records = flat_log_records(otel_collector)
        matching = [
            (scope, lr)
            for scope, lr in records
            if lr.body.stringValue
            and "cross signal stdlib warning" in lr.body.stringValue
        ]
        assert matching, (
            f"expected OTLP log record containing 'cross signal stdlib warning'; "
            f"got {len(records)} total records"
        )

    def test_stdlib_log_scope_is_apx_python(
        self, otel_collector: OtelCollector
    ) -> None:
        """Stdlib logs forwarded via _emit_log should have scope 'apx.python'."""
        records = flat_log_records(otel_collector)
        for scope, lr in records:
            if (
                lr.body.stringValue
                and "cross signal stdlib warning" in lr.body.stringValue
            ):
                assert scope.name == "apx.python", (
                    f"expected scope 'apx.python' for stdlib log; got {scope.name!r}"
                )
                return
        pytest.fail("stdlib log 'cross signal stdlib warning' not found")

    def test_stdlib_log_not_duplicated_as_span(
        self, otel_collector: OtelCollector
    ) -> None:
        """Stdlib logs should only appear as log records, not as spans."""
        spans = flat_spans(otel_collector)
        matching = [
            s for s in spans
            if "cross signal stdlib warning" in s.name
        ]
        assert not matching, (
            f"stdlib log should not produce a span; found {len(matching)} span(s)"
        )

    # ── Instrumentation scope: user metrics ───────────────────────────────

    def test_user_counter_scope_is_apx_user(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, m in flat_metrics_with_scope(otel_collector):
            if m.name == "test.cross_signal_counter":
                assert scope.name == "apx.user", (
                    f"expected scope 'apx.user' for counter; got {scope.name!r}"
                )
                return
        all_names = sorted({m.name for _, m in flat_metrics_with_scope(otel_collector)})
        pytest.fail(
            f"test.cross_signal_counter not found; available: {all_names}"
        )

    def test_user_histogram_scope_is_apx_user(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, m in flat_metrics_with_scope(otel_collector):
            if m.name == "test.cross_signal_histogram":
                assert scope.name == "apx.user", (
                    f"expected scope 'apx.user' for histogram; got {scope.name!r}"
                )
                return
        pytest.fail("test.cross_signal_histogram not found")

    # ── Trace correlation: all user spans share HTTP trace ────────────────

    def test_user_spans_share_http_trace_id(
        self, otel_collector: OtelCollector
    ) -> None:
        """All user spans from the cross-signal request should share one traceId."""
        user_span_names = {
            "test.cross_signal_span",
            "cross signal info log",
            "cross signal warn log",
        }
        trace_ids: dict[str, str] = {}
        for span in flat_spans(otel_collector):
            if span.name in user_span_names:
                trace_ids[span.name] = span.traceId

        assert len(trace_ids) == len(user_span_names), (
            f"not all user spans found; got {sorted(trace_ids.keys())}"
        )

        unique_traces = set(trace_ids.values())
        assert len(unique_traces) == 1, (
            f"all user spans should share one traceId; got {trace_ids}"
        )

    def test_log_info_span_is_zero_duration(
        self, otel_collector: OtelCollector
    ) -> None:
        """log.info() produces near-zero-duration spans (< 1ms)."""
        for span in flat_spans(otel_collector):
            if span.name == "cross signal info log":
                start = int(span.startTimeUnixNano)
                end = int(span.endTimeUnixNano)
                delta_us = (end - start) / 1_000
                assert delta_us < 1_000, (
                    f"log span should be near-zero-duration; "
                    f"delta={delta_us:.1f}µs"
                )
                return
        pytest.fail("'cross signal info log' span not found")
