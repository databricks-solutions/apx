"""Verify event_name support across both log emission paths.

Hits ``/api/telemetry/event-name`` which exercises:
- ``log.info()`` and ``log.warn()`` with ``event_name`` kwarg (instant spans)
- ``logging.getLogger().warning()`` with ``extra={"event_name": ...}`` (OTLP log record)

Also verifies that Rust-native framework logs carry a meaningful ``eventName``
via the ``opentelemetry-appender-tracing`` bridge (which reads the tracing
macro ``name:`` parameter).
"""

from __future__ import annotations

import pytest

from .conftest import (
    OtelCollector,
    flat_log_records,
    flat_spans,
    log_attrs,
    make_setup_fixture,
    span_attrs,
)


@pytest.mark.integration
class TestEventName:
    """Verify event_name propagation through spans and OTLP log records."""

    _setup = make_setup_fixture(
        "/api/telemetry/event-name", sleep_time=5, require_logs=True,
    )

    # ── log.info / log.warn produce spans with event.name attribute ──

    def test_log_info_span_has_event_name(
        self, otel_collector: OtelCollector
    ) -> None:
        """log.info(event_name='user.login') produces span with event.name attr."""
        for s in flat_spans(otel_collector):
            if s.name == "user logged in":
                attrs = span_attrs(s)
                assert attrs.get("event.name") == "user.login", (
                    f"expected event.name='user.login'; got {attrs}"
                )
                return
        pytest.fail("span 'user logged in' not found")

    def test_log_warn_span_has_event_name(
        self, otel_collector: OtelCollector
    ) -> None:
        """log.warn(event_name='rate_limit.warning') produces span with event.name attr."""
        for s in flat_spans(otel_collector):
            if s.name == "rate limit near":
                attrs = span_attrs(s)
                assert attrs.get("event.name") == "rate_limit.warning", (
                    f"expected event.name='rate_limit.warning'; got {attrs}"
                )
                return
        pytest.fail("span 'rate limit near' not found")

    # ── stdlib logging with extra={"event_name": ...} → OTLP log record ──

    def test_stdlib_log_has_event_name_attribute(
        self, otel_collector: OtelCollector
    ) -> None:
        """stdlib logging with extra={'event_name': ...} sets event.name on the OTLP log record."""
        records = flat_log_records(otel_collector)
        for _scope, lr in records:
            if lr.body.stringValue and "stdlib with event_name" in lr.body.stringValue:
                attrs = log_attrs(lr)
                assert attrs.get("event.name") == "stdlib.test_event", (
                    f"expected event.name='stdlib.test_event'; got attrs={attrs}"
                )
                return
        pytest.fail(
            f"OTLP log record containing 'stdlib with event_name' not found; "
            f"got {len(records)} total records"
        )

    # ── Rust-native framework logs have meaningful eventName ──

    def test_rust_framework_logs_have_event_name(
        self, otel_collector: OtelCollector
    ) -> None:
        """Rust framework tracing events should produce OTLP logs with non-empty eventName."""
        records = flat_log_records(otel_collector)
        framework_records = [
            (_scope, lr)
            for _scope, lr in records
            if _scope.name != "apx.python"
            and lr.eventName
            and lr.eventName.startswith("apx.")
        ]
        assert framework_records, (
            "expected at least one Rust framework log with eventName starting with 'apx.'; "
            f"scopes seen: {sorted({s.name for s, _ in records})}"
        )

    def test_rust_event_names_follow_convention(
        self, otel_collector: OtelCollector
    ) -> None:
        """All Rust framework eventNames should be dot-separated lowercase identifiers."""
        records = flat_log_records(otel_collector)
        for _scope, lr in records:
            if not lr.eventName or _scope.name == "apx.python":
                continue
            name = lr.eventName
            if not name.startswith("apx."):
                continue
            assert name == name.lower(), (
                f"eventName should be lowercase: {name!r}"
            )
            assert all(
                part.replace("_", "").isalnum()
                for part in name.split(".")
            ), f"eventName has invalid segment: {name!r}"
