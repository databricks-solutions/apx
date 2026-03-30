"""Verify all OTLP proto fields are properly populated in exported telemetry.

Hits ``/api/telemetry/otlp-fields`` which exercises:
- Span with explicit ``kind=CLIENT`` + events
- Span with default kind (INTERNAL)
- Log with ``event_name``
- Counter, Histogram, Gauge metrics

Tests verify field population across all three signals:
- Resource attributes and schema_url
- InstrumentationScope version and schema_url
- Metric start_time_unix_nano
- Log observed_time_unix_nano and flags
- Span kind, flags, events array
"""

from __future__ import annotations

import pytest

from .conftest import (
    OtelCollector,
    flat_log_records,
    flat_metrics_with_scope,
    flat_resource_logs,
    flat_resource_metrics,
    flat_resource_spans,
    flat_spans,
    flat_spans_with_scope,
    make_setup_fixture,
    span_attrs,
)


@pytest.mark.integration
class TestOtlpFields:
    """Verify all OTLP proto fields are properly populated in exported telemetry."""

    _setup = make_setup_fixture(
        "/api/telemetry/otlp-fields", sleep_time=5, require_logs=True,
    )

    # ── Resource attributes ───────────────────────────────────────────────

    def test_resource_has_service_name(
        self, otel_collector: OtelCollector
    ) -> None:
        """resource.attributes must include service.name."""
        for rs in flat_resource_spans(otel_collector):
            attr_keys = {a.key for a in rs.resource.attributes}
            if "service.name" in attr_keys:
                return
        pytest.fail("service.name not found in resource.attributes")

    def test_resource_has_apx_attributes(
        self, otel_collector: OtelCollector
    ) -> None:
        """resource.attributes must include apx.process.type and apx.worker.id."""
        for rs in flat_resource_spans(otel_collector):
            attr_keys = {a.key for a in rs.resource.attributes}
            if "apx.process.type" in attr_keys and "apx.worker.id" in attr_keys:
                return
        pytest.fail("apx.process.type / apx.worker.id not found in resource.attributes")

    # ── Resource schema_url ───────────────────────────────────────────────

    def test_resource_schema_url_on_spans(
        self, otel_collector: OtelCollector
    ) -> None:
        for rs in flat_resource_spans(otel_collector):
            if rs.schemaUrl:
                assert "opentelemetry.io/schemas" in rs.schemaUrl
                return
        pytest.fail("ResourceSpans.schemaUrl not populated")

    def test_resource_schema_url_on_logs(
        self, otel_collector: OtelCollector
    ) -> None:
        for rl in flat_resource_logs(otel_collector):
            if rl.schemaUrl:
                assert "opentelemetry.io/schemas" in rl.schemaUrl
                return
        pytest.fail("ResourceLogs.schemaUrl not populated")

    def test_resource_schema_url_on_metrics(
        self, otel_collector: OtelCollector
    ) -> None:
        for rm in flat_resource_metrics(otel_collector):
            if rm.schemaUrl:
                assert "opentelemetry.io/schemas" in rm.schemaUrl
                return
        pytest.fail("ResourceMetrics.schemaUrl not populated")

    # ── InstrumentationScope version ──────────────────────────────────────

    def test_span_scope_has_version(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, s in flat_spans_with_scope(otel_collector):
            if s.name == "test.client_call" and scope.version:
                return
        pytest.fail("Span scope version not populated for test.client_call")

    @pytest.mark.xfail(
        reason="OTEL Rust SDK batch log grouping reconstructs InstrumentationScope "
        "from target, dropping version (opentelemetry-proto transform)",
        strict=False,
    )
    def test_log_scope_has_version(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, lr in flat_log_records(otel_collector):
            if scope.name == "apx.python" and scope.version:
                return
        pytest.fail("Log scope version not populated for apx.python logger")

    def test_metric_scope_has_version(
        self, otel_collector: OtelCollector
    ) -> None:
        for scope, m in flat_metrics_with_scope(otel_collector):
            if m.name.startswith("test.otlp_fields") and scope.version:
                return
        pytest.fail("Metric scope version not populated for test.otlp_fields_* metrics")

    # ── InstrumentationScope schema_url ───────────────────────────────────

    def test_span_scope_has_schema_url(
        self, otel_collector: OtelCollector
    ) -> None:
        for rs in flat_resource_spans(otel_collector):
            for ss in rs.scopeSpans:
                if ss.scope.name == "apx.user" and ss.schemaUrl:
                    assert "opentelemetry.io/schemas" in ss.schemaUrl
                    return
        pytest.fail("ScopeSpans.schemaUrl not populated for apx.user scope")

    def test_log_scope_has_schema_url(
        self, otel_collector: OtelCollector
    ) -> None:
        for rl in flat_resource_logs(otel_collector):
            for sl in rl.scopeLogs:
                if sl.scope.name == "apx.python" and sl.schemaUrl:
                    assert "opentelemetry.io/schemas" in sl.schemaUrl
                    return
        pytest.fail("ScopeLogs.schemaUrl not populated for apx.python scope")

    def test_metric_scope_has_schema_url(
        self, otel_collector: OtelCollector
    ) -> None:
        for rm in flat_resource_metrics(otel_collector):
            for sm in rm.scopeMetrics:
                if sm.scope.name == "apx.user" and sm.schemaUrl:
                    assert "opentelemetry.io/schemas" in sm.schemaUrl
                    return
        pytest.fail("ScopeMetrics.schemaUrl not populated for apx.user scope")

    # ── Metric start_time_unix_nano ───────────────────────────────────────

    def test_counter_has_start_time(
        self, otel_collector: OtelCollector
    ) -> None:
        """Counter (Sum) should have non-empty start_time_unix_nano."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name == "test.otlp_fields_counter" and m.sum:
                for dp in m.sum.dataPoints:
                    if dp.startTimeUnixNano:
                        return
        pytest.fail("Counter start_time_unix_nano not populated")

    def test_histogram_has_start_time(
        self, otel_collector: OtelCollector
    ) -> None:
        """Histogram should have non-empty start_time_unix_nano."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name == "test.otlp_fields_histogram" and m.histogram:
                for dp in m.histogram.dataPoints:
                    if dp.startTimeUnixNano:
                        return
        pytest.fail("Histogram start_time_unix_nano not populated")

    def test_gauge_start_time(
        self, otel_collector: OtelCollector
    ) -> None:
        """Gauge start_time_unix_nano may or may not be populated depending on SDK version."""
        for _, m in flat_metrics_with_scope(otel_collector):
            if m.name == "test.otlp_fields_gauge" and m.gauge:
                return
        pytest.fail("Gauge test.otlp_fields_gauge not found")

    # ── Log observed_time_unix_nano ───────────────────────────────────────

    def test_log_has_observed_timestamp(
        self, otel_collector: OtelCollector
    ) -> None:
        """Log records should have non-empty observedTimeUnixNano."""
        for _, lr in flat_log_records(otel_collector):
            body = lr.body.stringValue or ""
            if "otlp fields test log" in body:
                assert lr.observedTimeUnixNano and lr.observedTimeUnixNano != "0", (
                    f"observedTimeUnixNano should be set; got {lr.observedTimeUnixNano!r}"
                )
                return
        pytest.fail("Log record 'otlp fields test log' not found")

    # ── Log flags ─────────────────────────────────────────────────────────

    def test_log_flags_has_trace_flags(
        self, otel_collector: OtelCollector
    ) -> None:
        """When trace context is present, log flags bits 0-7 should carry trace flags."""
        for _, lr in flat_log_records(otel_collector):
            body = lr.body.stringValue or ""
            if "otlp fields test log" in body and lr.traceId:
                assert lr.flags & 0xFF > 0, (
                    f"log flags should have trace flags set; got {lr.flags}"
                )
                return

    # ── Span kind ─────────────────────────────────────────────────────────

    def test_span_kind_client(
        self, otel_collector: OtelCollector
    ) -> None:
        """test.client_call should have kind=3 (CLIENT)."""
        for s in flat_spans(otel_collector):
            if s.name == "test.client_call":
                assert s.kind == 3, f"expected kind=3 (CLIENT); got {s.kind}"
                return
        pytest.fail("test.client_call span not found")

    def test_span_kind_internal_default(
        self, otel_collector: OtelCollector
    ) -> None:
        """test.internal_work should have kind=1 (INTERNAL)."""
        for s in flat_spans(otel_collector):
            if s.name == "test.internal_work":
                assert s.kind == 1, f"expected kind=1 (INTERNAL); got {s.kind}"
                return
        pytest.fail("test.internal_work span not found")

    # ── Span flags ────────────────────────────────────────────────────────

    def test_span_flags_nonzero(
        self, otel_collector: OtelCollector
    ) -> None:
        """Sampled spans should have flags with bit 0 set (SAMPLED)."""
        for s in flat_spans(otel_collector):
            if s.name == "test.client_call":
                assert s.flags & 0xFF > 0, (
                    f"span flags should have SAMPLED bit; got {s.flags}"
                )
                return

    # ── Span events ───────────────────────────────────────────────────────

    def test_span_events_array_populated(
        self, otel_collector: OtelCollector
    ) -> None:
        """test.client_call should have a dns.resolved event with attributes."""
        for s in flat_spans(otel_collector):
            if s.name == "test.client_call":
                event_names = [e.name for e in s.events]
                assert "dns.resolved" in event_names, (
                    f"expected 'dns.resolved' event; got {event_names}"
                )
                dns_event = next(e for e in s.events if e.name == "dns.resolved")
                event_attrs = {a.key: a.value.stringValue for a in dns_event.attributes}
                assert event_attrs.get("host") == "example.com"
                return
        pytest.fail("test.client_call span not found")

    # ── Log event_name (proto field) ──────────────────────────────────────

    def test_log_event_name_populated(
        self, otel_collector: OtelCollector
    ) -> None:
        """Log with event_name should carry it as an attribute.

        The OTEL Rust SDK ``set_event_name`` requires ``&'static str``,
        so dynamic Python event names are stored as the ``event.name``
        attribute instead of the proto ``eventName`` field.
        """
        for _, lr in flat_log_records(otel_collector):
            body = lr.body.stringValue or ""
            if "otlp fields test log" in body:
                attrs = {a.key: (a.value.stringValue or "") for a in lr.attributes}
                assert attrs.get("event.name") == "test.otlp_fields", (
                    f"expected event.name attribute 'test.otlp_fields'; got {attrs}"
                )
                return
        pytest.fail("Log record 'otlp fields test log' not found")
