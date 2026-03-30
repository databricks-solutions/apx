"""Verify error handling: exception events, span status, and log.exception.

Hits ``/api/telemetry/error-handling`` which exercises:

(a) ``test.erroring_span`` — raises ``ValueError`` inside ``with span()``.
(b) ``log.exception("caught runtime error")`` inside an ``except`` block.
(c) ``test.explicit_error`` — calls ``set_status(Error, ...)`` explicitly.
(d) ``test.clean_span`` — normal span for comparison.
"""

from __future__ import annotations

import pytest

from .conftest import (
    OtelCollector,
    find_span,
    make_setup_fixture,
    span_attrs,
)

OTEL_STATUS_UNSET = 0
OTEL_STATUS_ERROR = 2


@pytest.mark.integration
class TestErrorHandling:
    """Verify error attributes, exception events, and status codes."""

    _setup = make_setup_fixture("/api/telemetry/error-handling", sleep_time=3)

    # ── (a) Exception raised inside span context manager ──────────────────

    def test_erroring_span_has_error_status(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        assert span.status.code == OTEL_STATUS_ERROR, (
            f"expected status Error (2); got {span.status.code}"
        )

    def test_erroring_span_status_message(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        assert "deliberate test error" in span.status.message, (
            f"expected 'deliberate test error' in status message; "
            f"got {span.status.message!r}"
        )

    def test_erroring_span_has_exception_event(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        exc_events = [e for e in span.events if e.name == "exception"]
        assert exc_events, (
            f"expected 'exception' event on erroring span; "
            f"got events: {[e.name for e in span.events]}"
        )

    def test_erroring_span_exception_type(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        exc_event = next(e for e in span.events if e.name == "exception")
        attrs = {a.key: (a.value.stringValue or "") for a in exc_event.attributes}
        assert attrs.get("exception.type") == "ValueError", (
            f"expected exception.type='ValueError'; got {attrs.get('exception.type')!r}"
        )

    def test_erroring_span_exception_message(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        exc_event = next(e for e in span.events if e.name == "exception")
        attrs = {a.key: (a.value.stringValue or "") for a in exc_event.attributes}
        assert "deliberate test error" in attrs.get("exception.message", ""), (
            f"expected 'deliberate test error' in exception.message; "
            f"got {attrs.get('exception.message')!r}"
        )

    def test_erroring_span_exception_stacktrace(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.erroring_span")
        exc_event = next(e for e in span.events if e.name == "exception")
        attrs = {a.key: (a.value.stringValue or "") for a in exc_event.attributes}
        assert attrs.get("exception.stacktrace"), (
            "expected non-empty exception.stacktrace on erroring span"
        )

    # ── (b) log.exception() captures exception info ───────────────────────

    def test_log_exception_span_exists(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "caught runtime error")
        attrs = span_attrs(span)
        assert attrs.get("log.level") == "error"

    def test_log_exception_has_exception_type(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "caught runtime error")
        attrs = span_attrs(span)
        assert attrs.get("exception.type") == "RuntimeError", (
            f"expected exception.type='RuntimeError'; got {attrs.get('exception.type')!r}"
        )

    def test_log_exception_has_exception_message(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "caught runtime error")
        attrs = span_attrs(span)
        assert "log exception test" in attrs.get("exception.message", ""), (
            f"expected 'log exception test' in exception.message; "
            f"got {attrs.get('exception.message')!r}"
        )

    def test_log_exception_has_stacktrace(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "caught runtime error")
        attrs = span_attrs(span)
        assert attrs.get("exception.stacktrace"), (
            "expected non-empty exception.stacktrace on log.exception span"
        )

    def test_log_exception_has_source_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "caught runtime error")
        attrs = span_attrs(span)
        assert attrs.get("source") == "test"

    # ── (c) Explicit set_status(Error) ────────────────────────────────────

    def test_explicit_error_span_has_error_status(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.explicit_error")
        assert span.status.code == OTEL_STATUS_ERROR, (
            f"expected status Error (2); got {span.status.code}"
        )

    def test_explicit_error_span_status_message(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.explicit_error")
        assert "manually set error" in span.status.message, (
            f"expected 'manually set error' in status; got {span.status.message!r}"
        )

    # ── (d) Clean span — no error ─────────────────────────────────────────

    def test_clean_span_has_unset_status(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.clean_span")
        assert span.status.code == OTEL_STATUS_UNSET, (
            f"clean span should have status Unset (0); got {span.status.code}"
        )

    def test_clean_span_has_no_exception_events(
        self, otel_collector: OtelCollector
    ) -> None:
        span = find_span(otel_collector, "test.clean_span")
        exc_events = [e for e in span.events if e.name == "exception"]
        assert not exc_events, (
            f"clean span should have no exception events; found {len(exc_events)}"
        )
