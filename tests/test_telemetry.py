"""Unit tests for the apx.telemetry Python module.

Tests cover:
- Metric toggle models (defaults, overrides, serialization)
- Instrumentation models and the discriminated union
- configure() / _get_config() merge logic
- metric_catalog() introspection from Rust
- Unit class constants and custom unit creation
- span context manager, async context manager, and decorator
- log namespace methods at all severity levels
- Counter, Histogram, Gauge wrappers (construction + invocation)
- StatusCode enum values
- _resolve_identity() worker/supervisor logic
"""

from __future__ import annotations

import asyncio

import pytest

from apx.telemetry import (
    ApxInstrumentation,
    ApxMetrics,
    Attribute,
    CaptureHeaders,
    Configuration,
    Counter,
    Gauge,
    Histogram,
    HttpInstrumentation,
    HttpMetrics,
    MetricDefinition,
    ProcessInstrumentation,
    ProcessMetrics,
    Resource,
    SpanKind,
    StatusCode,
    SystemInstrumentation,
    SystemMetrics,
    Unit,
    configure,
    log,
    metric_catalog,
    span,
)


# ── Metric toggle model defaults ─────────────────────────────────────────


class TestSystemMetricsDefaults:
    def test_defaults(self) -> None:
        m = SystemMetrics()
        assert m.cpu is True
        assert m.memory is True
        assert m.paging is False
        assert m.disk_io is False
        assert m.network_io is False

    def test_override_single(self) -> None:
        m = SystemMetrics(cpu=False)
        assert m.cpu is False
        assert m.memory is True

    def test_enable_all(self) -> None:
        m = SystemMetrics(
            cpu=True, memory=True, paging=True, disk_io=True, network_io=True
        )
        assert all([m.cpu, m.memory, m.paging, m.disk_io, m.network_io])

    def test_model_dump_roundtrip(self) -> None:
        m = SystemMetrics(paging=True)
        data = m.model_dump()
        assert data["paging"] is True
        assert data["cpu"] is True
        restored = SystemMetrics.model_validate(data)
        assert restored == m


class TestProcessMetricsDefaults:
    def test_defaults(self) -> None:
        m = ProcessMetrics()
        assert m.cpu is True
        assert m.memory is False
        assert m.threads is False

    def test_override(self) -> None:
        m = ProcessMetrics(memory=True, threads=True)
        assert m.cpu is True
        assert m.memory is True
        assert m.threads is True

    def test_model_dump_roundtrip(self) -> None:
        m = ProcessMetrics(threads=True)
        data = m.model_dump()
        restored = ProcessMetrics.model_validate(data)
        assert restored == m


class TestHttpMetricsDefaults:
    def test_defaults(self) -> None:
        m = HttpMetrics()
        assert m.server_request_duration is True
        assert m.server_active_requests is True

    def test_disable_both(self) -> None:
        m = HttpMetrics(server_request_duration=False, server_active_requests=False)
        assert m.server_request_duration is False
        assert m.server_active_requests is False


class TestApxMetricsDefaults:
    def test_all_enabled_by_default(self) -> None:
        m = ApxMetrics()
        assert m.dispatch_body_collect is True
        assert m.dispatch_crossbeam_send is True
        assert m.dispatch_response_wait is True
        assert m.dispatch_total is True
        assert m.asgi_receive_build is True
        assert m.asgi_send_parse is True
        assert m.dispatch_pickup_delay is True
        assert m.dispatch_materialize is True
        assert m.dispatch_queue_depth is True

    def test_disable_selective(self) -> None:
        m = ApxMetrics(dispatch_total=False, asgi_send_parse=False)
        assert m.dispatch_total is False
        assert m.asgi_send_parse is False
        assert m.dispatch_body_collect is True


# ── Instrumentation models ────────────────────────────────────────────────


class TestInstrumentationModels:
    def test_http_defaults(self) -> None:
        h = HttpInstrumentation()
        assert h.type == "http"
        assert h.enabled is True
        assert h.capture_headers == CaptureHeaders()
        assert h.metrics.server_request_duration is True

    def test_http_with_headers(self) -> None:
        h = HttpInstrumentation(
            capture_headers=CaptureHeaders(
                request=["x-request-id"],
                sanitize=["authorization"],
            )
        )
        assert h.capture_headers.request == ["x-request-id"]
        assert h.capture_headers.sanitize == ["authorization"]
        assert h.capture_headers.response == []

    def test_system_defaults(self) -> None:
        s = SystemInstrumentation()
        assert s.type == "system"
        assert s.enabled is True
        assert s.metrics.cpu is True

    def test_process_defaults(self) -> None:
        p = ProcessInstrumentation()
        assert p.type == "process"
        assert p.enabled is True
        assert p.metrics.cpu is True
        assert p.metrics.memory is False

    def test_apx_defaults(self) -> None:
        a = ApxInstrumentation()
        assert a.type == "apx"
        assert a.enabled is True
        assert a.metrics.dispatch_total is True

    def test_discriminated_union_from_dict(self) -> None:
        """Configuration parses typed dicts via the discriminated union."""
        config = Configuration(
            instrumentations=[
                HttpInstrumentation(enabled=False),
                SystemInstrumentation(metrics=SystemMetrics(paging=True)),
                ProcessInstrumentation(metrics=ProcessMetrics(threads=True)),
                ApxInstrumentation(metrics=ApxMetrics(dispatch_total=True)),
            ]
        )
        types = [i.type for i in config.instrumentations]
        assert types == ["http", "system", "process", "apx"]

        http = config.instrumentations[0]
        assert isinstance(http, HttpInstrumentation)
        assert http.enabled is False

        system = config.instrumentations[1]
        assert isinstance(system, SystemInstrumentation)
        assert system.metrics.paging is True
        assert system.metrics.cpu is True

        process = config.instrumentations[2]
        assert isinstance(process, ProcessInstrumentation)
        assert process.metrics.threads is True

        apx = config.instrumentations[3]
        assert isinstance(apx, ApxInstrumentation)
        assert apx.metrics.dispatch_total is True


# ── APX_PERF conditional defaults ─────────────────────────────────────────


class TestApxPerfToggle:
    """Verify APX dispatch metrics are only in defaults when APX_PERF is set."""

    def setup_method(self) -> None:
        configure(Configuration())

    def test_apx_not_in_defaults_without_env(self) -> None:
        """Without APX_PERF, default config has no 'apx' instrumentation."""
        from apx.telemetry import _get_config

        config = _get_config()
        types = [i["type"] for i in config["instrumentations"]]
        assert "apx" not in types

    def test_apx_added_via_user_configure(self) -> None:
        """User can still add APX instrumentation explicitly via configure()."""
        from apx.telemetry import _get_config

        configure(
            Configuration(
                instrumentations=[
                    ApxInstrumentation(metrics=ApxMetrics(dispatch_total=True))
                ]
            )
        )
        config = _get_config()
        types = [i["type"] for i in config["instrumentations"]]
        assert "apx" in types
        apx = next(i for i in config["instrumentations"] if i["type"] == "apx")
        assert apx["metrics"]["dispatch_total"] is True

    def test_apx_in_defaults_with_env(self) -> None:
        """With APX_PERF=1, default config includes 'apx' instrumentation."""
        import subprocess
        import sys

        result = subprocess.run(
            [
                sys.executable,
                "-c",
                "from apx.telemetry import _get_config; "
                "types = [i['type'] for i in _get_config()['instrumentations']]; "
                "assert 'apx' in types, f'expected apx in {types}'; "
                "print('OK')",
            ],
            env={**__import__("os").environ, "APX_PERF": "1"},
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, (
            f"subprocess failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
        assert "OK" in result.stdout


# ── configure() / _get_config() merge logic ───────────────────────────────


class TestConfigureMerge:
    """Test the merge-by-type semantics of configure()."""

    def setup_method(self) -> None:
        """Reset to defaults before each test."""
        configure(Configuration())

    def test_default_config_has_http_system_process(self) -> None:
        from apx.telemetry import _get_config

        config = _get_config()
        types = [i["type"] for i in config["instrumentations"]]
        assert "http" in types
        assert "system" in types
        assert "process" in types

    def test_override_replaces_by_type(self) -> None:
        from apx.telemetry import _get_config

        configure(
            Configuration(instrumentations=[SystemInstrumentation(enabled=False)])
        )
        config = _get_config()
        system_entries = [
            i for i in config["instrumentations"] if i["type"] == "system"
        ]
        assert len(system_entries) == 1
        assert system_entries[0]["enabled"] is False

    def test_override_preserves_unmentioned_defaults(self) -> None:
        from apx.telemetry import _get_config

        configure(
            Configuration(
                instrumentations=[
                    ApxInstrumentation(metrics=ApxMetrics(dispatch_total=True))
                ]
            )
        )
        config = _get_config()
        types = [i["type"] for i in config["instrumentations"]]
        assert "http" in types
        assert "system" in types
        assert "process" in types
        assert "apx" in types

    def test_override_metrics_fields(self) -> None:
        from apx.telemetry import _get_config

        configure(
            Configuration(
                instrumentations=[
                    SystemInstrumentation(metrics=SystemMetrics(cpu=False, paging=True))
                ]
            )
        )
        config = _get_config()
        system = next(i for i in config["instrumentations"] if i["type"] == "system")
        assert system["metrics"]["cpu"] is False
        assert system["metrics"]["paging"] is True
        assert system["metrics"]["memory"] is True

    def test_full_serialization_roundtrip(self) -> None:
        from apx.telemetry import _get_config

        configure(
            Configuration(
                instrumentations=[
                    HttpInstrumentation(
                        capture_headers=CaptureHeaders(request=["x-trace-id"]),
                        metrics=HttpMetrics(server_active_requests=False),
                    ),
                    ProcessInstrumentation(
                        metrics=ProcessMetrics(memory=True),
                    ),
                ]
            )
        )
        config = _get_config()
        http = next(i for i in config["instrumentations"] if i["type"] == "http")
        assert http["capture_headers"]["request"] == ["x-trace-id"]
        assert http["metrics"]["server_active_requests"] is False
        assert http["metrics"]["server_request_duration"] is True

        process = next(i for i in config["instrumentations"] if i["type"] == "process")
        assert process["metrics"]["memory"] is True
        assert process["metrics"]["cpu"] is True


# ── metric_catalog() introspection ────────────────────────────────────────


EXPECTED_GROUPS = {"system", "process", "http", "apx"}
EXPECTED_SCOPES = {"supervisor", "worker", "both"}

EXPECTED_SYSTEM_METRICS = {
    "system.cpu.utilization",
    "system.memory.utilization",
    "system.paging.utilization",
    "system.disk.io",
    "system.network.io",
}

EXPECTED_PROCESS_METRICS = {
    "process.cpu.utilization",
    "process.memory.usage",
    "process.thread.count",
}

EXPECTED_HTTP_METRICS = {
    "http.server.request.duration",
    "http.server.active_requests",
}

EXPECTED_APX_METRICS = {
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


class TestMetricCatalog:
    def test_returns_list(self) -> None:
        catalog = metric_catalog()
        assert isinstance(catalog, list)

    def test_count(self) -> None:
        catalog = metric_catalog()
        assert len(catalog) == 19

    def test_entry_type(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            assert isinstance(entry, MetricDefinition)

    def test_entry_fields_are_strings(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            assert isinstance(entry.name, str) and entry.name
            assert isinstance(entry.description, str) and entry.description
            assert isinstance(entry.unit, str) and entry.unit
            assert isinstance(entry.group, str) and entry.group
            assert isinstance(entry.scope, str) and entry.scope

    def test_groups_are_valid(self) -> None:
        catalog = metric_catalog()
        groups = {e.group for e in catalog}
        assert groups == EXPECTED_GROUPS

    def test_scopes_are_valid(self) -> None:
        catalog = metric_catalog()
        scopes = {e.scope for e in catalog}
        assert scopes == EXPECTED_SCOPES

    def test_system_metrics_present(self) -> None:
        catalog = metric_catalog()
        system_names = {e.name for e in catalog if e.group == "system"}
        assert system_names == EXPECTED_SYSTEM_METRICS

    def test_system_metrics_supervisor_scope(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            if entry.group == "system":
                assert entry.scope == "supervisor", (
                    f"{entry.name} should be supervisor-scoped"
                )

    def test_process_metrics_present(self) -> None:
        catalog = metric_catalog()
        process_names = {e.name for e in catalog if e.group == "process"}
        assert process_names == EXPECTED_PROCESS_METRICS

    def test_process_metrics_both_scope(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            if entry.group == "process":
                assert entry.scope == "both", f"{entry.name} should be both-scoped"

    def test_http_metrics_present(self) -> None:
        catalog = metric_catalog()
        http_names = {e.name for e in catalog if e.group == "http"}
        assert http_names == EXPECTED_HTTP_METRICS

    def test_http_metrics_worker_scope(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            if entry.group == "http":
                assert entry.scope == "worker", f"{entry.name} should be worker-scoped"

    def test_apx_metrics_present(self) -> None:
        catalog = metric_catalog()
        apx_names = {e.name for e in catalog if e.group == "apx"}
        assert apx_names == EXPECTED_APX_METRICS

    def test_apx_metrics_worker_scope(self) -> None:
        catalog = metric_catalog()
        for entry in catalog:
            if entry.group == "apx":
                assert entry.scope == "worker", f"{entry.name} should be worker-scoped"

    def test_all_names_unique(self) -> None:
        catalog = metric_catalog()
        names = [e.name for e in catalog]
        assert len(names) == len(set(names)), "duplicate metric names in catalog"

    def test_repr(self) -> None:
        catalog = metric_catalog()
        r = repr(catalog[0])
        assert "MetricDefinition" in r
        assert "name=" in r
        assert "group=" in r
        assert "scope=" in r

    def test_completeness_against_all_known_metrics(self) -> None:
        """Every known framework metric name appears in the catalog."""
        catalog = metric_catalog()
        all_names = {e.name for e in catalog}
        expected = (
            EXPECTED_SYSTEM_METRICS
            | EXPECTED_PROCESS_METRICS
            | EXPECTED_HTTP_METRICS
            | EXPECTED_APX_METRICS
        )
        assert all_names == expected


# ── Metric unit + description correctness ─────────────────────────────────

VALID_UCUM_UNITS = {"1", "s", "ms", "us", "By", "kBy", "MBy", "%"}

EXPECTED_UNITS: dict[str, str] = {
    "system.cpu.utilization": "1",
    "system.memory.utilization": "1",
    "system.paging.utilization": "1",
    "system.disk.io": "By",
    "system.network.io": "By",
    "process.cpu.utilization": "1",
    "process.memory.usage": "By",
    "process.thread.count": "1",
    "http.server.request.duration": "s",
    "http.server.active_requests": "1",
    "apx.dispatch.body_collect.duration": "us",
    "apx.dispatch.crossbeam_send.duration": "us",
    "apx.dispatch.response_wait.duration": "us",
    "apx.dispatch.total.duration": "us",
    "apx.asgi.receive_build.duration": "us",
    "apx.asgi.send_parse.duration": "us",
    "apx.dispatch.pickup_delay.duration": "us",
    "apx.dispatch.materialize.duration": "us",
    "apx.dispatch.queue_depth": "1",
}

EXPECTED_DESCRIPTIONS: dict[str, str] = {
    "system.cpu.utilization": "System-wide CPU utilization as a fraction",
    "system.memory.utilization": "Fraction of available memory used",
    "system.paging.utilization": "Fraction of paging (swap) space used",
    "system.disk.io": "Cumulative disk I/O in bytes",
    "system.network.io": "Cumulative network I/O in bytes",
    "process.cpu.utilization": "Process CPU utilization as a fraction of one core",
    "process.memory.usage": "Process resident memory in bytes",
    "process.thread.count": "Number of threads in the process",
    "http.server.request.duration": "Duration of HTTP server requests",
    "http.server.active_requests": "Number of in-flight HTTP server requests",
    "apx.dispatch.body_collect.duration": "Time to collect the request body from the network stream",
    "apx.dispatch.crossbeam_send.duration": "Time to push the request slot to the crossbeam channel and signal wakeup",
    "apx.dispatch.response_wait.duration": "Time waiting for the Python handler to produce a response",
    "apx.dispatch.total.duration": "Total dispatch duration from body collect start to response ready",
    "apx.asgi.receive_build.duration": "Time to build the ASGI receive dict for the Python handler",
    "apx.asgi.send_parse.duration": "Time to parse the ASGI send event dict from the Python handler",
    "apx.dispatch.pickup_delay.duration": "Time from slot creation to asyncio thread pickup",
    "apx.dispatch.materialize.duration": "Time to build ASGI scope and receive/send callables",
    "apx.dispatch.queue_depth": "Pending request slots in the crossbeam channel at drain time",
}


class TestMetricUnits:
    """Verify every metric has a correct UCUM unit — catches missing .with_unit() calls."""

    def test_all_units_non_empty(self) -> None:
        for entry in metric_catalog():
            assert entry.unit, f"{entry.name} has empty unit"

    def test_all_units_are_valid_ucum(self) -> None:
        for entry in metric_catalog():
            assert entry.unit in VALID_UCUM_UNITS, (
                f"{entry.name} has unexpected unit {entry.unit!r}"
            )

    def test_units_match_expected_values(self) -> None:
        for entry in metric_catalog():
            expected = EXPECTED_UNITS.get(entry.name)
            assert expected is not None, f"no expected unit for {entry.name}"
            assert entry.unit == expected, (
                f"{entry.name}: unit={entry.unit!r}, expected {expected!r}"
            )

    def test_http_active_requests_unit(self) -> None:
        """Regression: http.server.active_requests was missing .with_unit()."""
        entry = next(
            e for e in metric_catalog() if e.name == "http.server.active_requests"
        )
        assert entry.unit == "1"


class TestMetricDescriptions:
    """Verify every metric has a meaningful description — catches missing .with_description()."""

    def test_all_descriptions_non_empty(self) -> None:
        for entry in metric_catalog():
            assert entry.description, f"{entry.name} has empty description"

    def test_no_placeholder_descriptions(self) -> None:
        """No metric should have a placeholder like '-' or 'TODO'."""
        for entry in metric_catalog():
            assert entry.description not in ("-", "TODO", "N/A", "n/a"), (
                f"{entry.name} has placeholder description {entry.description!r}"
            )

    def test_descriptions_match_expected_values(self) -> None:
        for entry in metric_catalog():
            expected = EXPECTED_DESCRIPTIONS.get(entry.name)
            assert expected is not None, f"no expected description for {entry.name}"
            assert entry.description == expected, (
                f"{entry.name}: description={entry.description!r}, expected {expected!r}"
            )

    def test_http_active_requests_description(self) -> None:
        """Regression: http.server.active_requests was missing .with_description()."""
        entry = next(
            e for e in metric_catalog() if e.name == "http.server.active_requests"
        )
        assert entry.description == "Number of in-flight HTTP server requests"

    def test_descriptions_are_sentence_fragments(self) -> None:
        """Descriptions should start with an uppercase letter and not end with a period."""
        for entry in metric_catalog():
            assert entry.description[0].isupper(), (
                f"{entry.name}: description should start uppercase: {entry.description!r}"
            )
            assert not entry.description.endswith("."), (
                f"{entry.name}: description should not end with period: {entry.description!r}"
            )


# ── Unit class ────────────────────────────────────────────────────────────


class TestUnit:
    """Verify Unit constants and string inheritance."""

    def test_is_str_subclass(self) -> None:
        assert isinstance(Unit.seconds, str)

    def test_seconds(self) -> None:
        assert Unit.seconds == "s"

    def test_milliseconds(self) -> None:
        assert Unit.milliseconds == "ms"

    def test_bytes(self) -> None:
        assert Unit.bytes == "By"

    def test_kilobytes(self) -> None:
        assert Unit.kilobytes == "kBy"

    def test_megabytes(self) -> None:
        assert Unit.megabytes == "MBy"

    def test_requests(self) -> None:
        assert Unit.requests == "1"

    def test_ratio(self) -> None:
        assert Unit.ratio == "1"

    def test_percent(self) -> None:
        assert Unit.percent == "%"

    def test_dimensionless(self) -> None:
        assert Unit.dimensionless == "1"

    def test_custom_unit(self) -> None:
        custom = Unit("widgets")
        assert custom == "widgets"
        assert isinstance(custom, Unit)

    def test_unit_usable_as_string(self) -> None:
        assert f"duration in {Unit.seconds}" == "duration in s"


# ── StatusCode enum ───────────────────────────────────────────────────────


class TestStatusCode:
    """Verify StatusCode enum values exposed from Rust."""

    def test_ok_value(self) -> None:
        assert StatusCode.Ok == 0

    def test_error_value(self) -> None:
        assert StatusCode.Error == 1

    def test_distinct(self) -> None:
        assert StatusCode.Ok != StatusCode.Error


# ── span class ────────────────────────────────────────────────────────────


class TestSpan:
    """Verify the span context manager, async context manager, and decorator."""

    def test_sync_context_manager(self) -> None:
        with span("test.sync_cm") as handle:
            assert handle is not None

    def test_sync_context_manager_with_attributes(self) -> None:
        with span("test.attrs", key="value", count=42) as handle:
            assert handle is not None

    def test_async_context_manager(self) -> None:
        async def _run() -> object:
            async with span("test.async_cm") as handle:
                return handle

        handle = asyncio.get_event_loop().run_until_complete(_run())
        assert handle is not None

    def test_decorator_sync(self) -> None:
        @span("test.sync_dec")
        def decorated() -> str:
            return "ok"

        assert decorated() == "ok"

    def test_decorator_async(self) -> None:
        @span("test.async_dec")
        async def decorated() -> str:
            return "ok"

        result = asyncio.get_event_loop().run_until_complete(decorated())
        assert result == "ok"

    def test_nested_spans(self) -> None:
        with span("test.outer"):
            with span("test.inner"):
                pass

    def test_exit_returns_false(self) -> None:
        """__exit__ should not suppress exceptions (returns False)."""
        s = span("test.exit_false")
        s.__enter__()
        assert s.__exit__(None, None, None) is False

    def test_merged_attrs_include_identity(self) -> None:
        """_merged_attrs should include _IDENTITY_ATTRS."""
        s = span("test.identity", custom="val")
        merged = s._merged_attrs()
        assert "apx.process.type" in merged
        assert "apx.worker.id" in merged
        assert merged["custom"] == "val"


# ── log namespace ─────────────────────────────────────────────────────────


class TestLog:
    """Verify all log level methods execute without error."""

    def test_trace(self) -> None:
        log.trace("trace msg", key="val")

    def test_debug(self) -> None:
        log.debug("debug msg")

    def test_info(self) -> None:
        log.info("info msg", status=200)

    def test_notice(self) -> None:
        log.notice("notice msg")

    def test_warn(self) -> None:
        log.warn("warn msg", threshold=100)

    def test_error(self) -> None:
        log.error("error msg")

    def test_fatal(self) -> None:
        log.fatal("fatal msg")

    def test_exception_in_handler(self) -> None:
        """log.exception captures the current exception context."""
        try:
            raise ValueError("test error")
        except ValueError:
            log.exception("caught error", detail="extra")

    def test_exception_without_active_exc(self) -> None:
        """log.exception outside except block still works (no active exc)."""
        log.exception("no active exception")

    def test_info_with_event_name(self) -> None:
        """log.info accepts event_name keyword argument."""
        log.info("user signed in", event_name="user.login", uid="42")

    def test_all_levels_accept_event_name(self) -> None:
        """Every log level method accepts event_name."""
        log.trace("t", event_name="test.trace")
        log.debug("d", event_name="test.debug")
        log.info("i", event_name="test.info")
        log.notice("n", event_name="test.notice")
        log.warn("w", event_name="test.warn")
        log.error("e", event_name="test.error")
        log.fatal("f", event_name="test.fatal")
        try:
            raise ValueError("exc")
        except ValueError:
            log.exception("x", event_name="test.exception")

    def test_event_name_none_by_default(self) -> None:
        """event_name defaults to None — omitting it must not raise."""
        log.info("no event name")


# ── Counter wrapper ───────────────────────────────────────────────────────


class TestCounter:
    """Verify Counter construction and invocation."""

    def test_constructor_stores_fields(self) -> None:
        c = Counter("test.counter", description="desc", unit=Unit.requests)
        assert c.name == "test.counter"
        assert c.description == "desc"
        assert c.unit == "1"

    def test_inc_default(self) -> None:
        c = Counter("test.counter.default")
        c.inc()

    def test_inc_value(self) -> None:
        c = Counter("test.counter.val")
        c.inc(5)

    def test_inc_with_attributes(self) -> None:
        c = Counter("test.counter.attrs")
        c.inc(1, attributes={"method": "GET"})

    def test_unit_accepts_string(self) -> None:
        c = Counter("test.counter.str_unit", unit="widgets")
        assert c.unit == "widgets"


# ── Histogram wrapper ─────────────────────────────────────────────────────


class TestHistogram:
    """Verify Histogram construction and invocation."""

    def test_constructor_stores_fields(self) -> None:
        h = Histogram("test.histo", description="latency", unit=Unit.milliseconds)
        assert h.name == "test.histo"
        assert h.description == "latency"
        assert h.unit == "ms"

    def test_observe(self) -> None:
        h = Histogram("test.histo.obs")
        h.observe(42.0)

    def test_observe_with_attributes(self) -> None:
        h = Histogram("test.histo.attrs")
        h.observe(99.0, attributes={"endpoint": "/api"})


# ── Gauge wrapper ─────────────────────────────────────────────────────────


class TestGauge:
    """Verify Gauge construction and invocation."""

    def test_constructor_stores_fields(self) -> None:
        g = Gauge("test.gauge", description="active", unit=Unit.dimensionless)
        assert g.name == "test.gauge"
        assert g.description == "active"
        assert g.unit == "1"

    def test_set(self) -> None:
        g = Gauge("test.gauge.set")
        g.set(7.0)

    def test_set_with_attributes(self) -> None:
        g = Gauge("test.gauge.attrs")
        g.set(3.0, attributes={"pool": "main"})


# ── _resolve_identity ─────────────────────────────────────────────────────


class TestResolveIdentity:
    """Verify worker vs supervisor identity resolution from env vars."""

    def test_supervisor_when_no_worker_env(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv("APX_WORKER_ID", raising=False)
        monkeypatch.delenv("APX_WORKER_NONCE", raising=False)
        from apx.telemetry import _resolve_identity

        result = _resolve_identity()
        assert result["apx.process.type"] == "supervisor"
        assert result["apx.worker.id"] == "supervisor"

    def test_worker_with_id(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("APX_WORKER_ID", "3")
        from apx.telemetry import _resolve_identity

        result = _resolve_identity()
        assert result["apx.process.type"] == "worker"
        assert result["apx.worker.id"] == "worker-3"

    def test_supervisor_when_nonce_only(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """With APX_WORKER_NONCE but no APX_WORKER_ID, falls back to supervisor."""
        monkeypatch.delenv("APX_WORKER_ID", raising=False)
        monkeypatch.setenv("APX_WORKER_NONCE", "abc")
        from apx.telemetry import _resolve_identity

        result = _resolve_identity()
        assert result["apx.process.type"] == "supervisor"
        assert result["apx.worker.id"] == "supervisor"


# ── Attribute model ───────────────────────────────────────────────────────


class TestAttribute:
    """Verify Attribute Pydantic model for resource configuration."""

    def test_construction(self) -> None:
        a = Attribute(key="env", value="prod")
        assert a.key == "env"
        assert a.value == "prod"

    def test_model_dump(self) -> None:
        a = Attribute(key="team", value="platform")
        data = a.model_dump()
        assert data == {"key": "team", "value": "platform"}

    def test_roundtrip(self) -> None:
        a = Attribute(key="k", value="v")
        restored = Attribute.model_validate(a.model_dump())
        assert restored == a


# ── Resource model ────────────────────────────────────────────────────────


class TestResource:
    """Verify Resource Pydantic model for OTLP resource configuration."""

    def test_defaults_empty(self) -> None:
        r = Resource()
        assert r.attributes == []
        assert r.schema_url is None

    def test_with_attributes(self) -> None:
        r = Resource(attributes=[Attribute(key="k", value="v")])
        assert len(r.attributes) == 1
        assert r.attributes[0].key == "k"

    def test_with_schema_url(self) -> None:
        r = Resource(schema_url="https://example.com/schema")
        assert r.schema_url == "https://example.com/schema"

    def test_model_dump(self) -> None:
        r = Resource(
            attributes=[Attribute(key="env", value="staging")],
            schema_url="https://example.com",
        )
        data = r.model_dump()
        assert data["attributes"][0]["key"] == "env"
        assert data["schema_url"] == "https://example.com"


# ── Configuration resource field ──────────────────────────────────────────


class TestConfigurationResource:
    """Verify resource field in Configuration model."""

    def test_default_resource_is_empty(self) -> None:
        c = Configuration()
        assert c.resource.attributes == []
        assert c.resource.schema_url is None

    def test_resource_in_config(self) -> None:
        c = Configuration(
            resource=Resource(
                attributes=[Attribute(key="env", value="staging")]
            )
        )
        assert len(c.resource.attributes) == 1
        assert c.resource.attributes[0].key == "env"

    def test_get_config_includes_resource(self) -> None:
        configure(
            Configuration(
                resource=Resource(
                    attributes=[Attribute(key="team", value="platform")]
                )
            )
        )
        from apx.telemetry import _get_config

        config = _get_config()
        assert "resource" in config
        assert config["resource"]["attributes"][0]["key"] == "team"
        assert config["resource"]["attributes"][0]["value"] == "platform"
        configure(Configuration())

    def test_get_config_resource_schema_url(self) -> None:
        configure(
            Configuration(
                resource=Resource(schema_url="https://custom.schema")
            )
        )
        from apx.telemetry import _get_config

        config = _get_config()
        assert config["resource"]["schema_url"] == "https://custom.schema"
        configure(Configuration())

    def test_get_config_no_schema_url_by_default(self) -> None:
        configure(Configuration())
        from apx.telemetry import _get_config

        config = _get_config()
        assert "schema_url" not in config["resource"]


# ── SpanKind enum ─────────────────────────────────────────────────────────


class TestSpanKind:
    """Verify SpanKind enum values match OTLP proto definition."""

    def test_internal_value(self) -> None:
        assert SpanKind.INTERNAL == 1

    def test_server_value(self) -> None:
        assert SpanKind.SERVER == 2

    def test_client_value(self) -> None:
        assert SpanKind.CLIENT == 3

    def test_producer_value(self) -> None:
        assert SpanKind.PRODUCER == 4

    def test_consumer_value(self) -> None:
        assert SpanKind.CONSUMER == 5

    def test_span_accepts_kind(self) -> None:
        with span("test.with_kind", kind=SpanKind.CLIENT):
            pass

    def test_span_default_kind_is_internal(self) -> None:
        s = span("test.default_kind")
        assert s._kind == SpanKind.INTERNAL
        with s:
            pass

    def test_span_kind_propagated_to_decorator(self) -> None:
        @span("test.dec_kind", kind=SpanKind.SERVER)
        def decorated() -> str:
            return "ok"

        assert decorated() == "ok"
