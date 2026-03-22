"""Unit tests for the apx.telemetry Python module.

Tests cover:
- Metric toggle models (defaults, overrides, serialization)
- Instrumentation models and the discriminated union
- configure() / _get_config() merge logic
- metric_catalog() introspection from Rust
"""

from __future__ import annotations

import pytest

from apx.telemetry import (
    ApxInstrumentation,
    ApxMetrics,
    CaptureHeaders,
    Configuration,
    HttpInstrumentation,
    HttpMetrics,
    MetricDefinition,
    ProcessInstrumentation,
    ProcessMetrics,
    SystemInstrumentation,
    SystemMetrics,
    configure,
    metric_catalog,
)


# ── Metric toggle model defaults ─────────────────────────────────────────


class TestSystemMetricsDefaults:
    def test_defaults(self) -> None:
        m = SystemMetrics()
        assert m.cpu is True
        assert m.memory is True
        assert m.swap is False
        assert m.disk_io is False
        assert m.network_io is False

    def test_override_single(self) -> None:
        m = SystemMetrics(cpu=False)
        assert m.cpu is False
        assert m.memory is True

    def test_enable_all(self) -> None:
        m = SystemMetrics(
            cpu=True, memory=True, swap=True, disk_io=True, network_io=True
        )
        assert all([m.cpu, m.memory, m.swap, m.disk_io, m.network_io])

    def test_model_dump_roundtrip(self) -> None:
        m = SystemMetrics(swap=True)
        data = m.model_dump()
        assert data["swap"] is True
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
    def test_all_disabled_by_default(self) -> None:
        m = ApxMetrics()
        assert m.dispatch_body_collect is False
        assert m.dispatch_crossbeam_send is False
        assert m.dispatch_response_wait is False
        assert m.dispatch_total is False
        assert m.asgi_receive_build is False
        assert m.asgi_send_parse is False

    def test_enable_selective(self) -> None:
        m = ApxMetrics(dispatch_total=True, asgi_send_parse=True)
        assert m.dispatch_total is True
        assert m.asgi_send_parse is True
        assert m.dispatch_body_collect is False


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
        assert s.interval_seconds == 15.0
        assert s.metrics.cpu is True

    def test_system_custom_interval(self) -> None:
        s = SystemInstrumentation(interval_seconds=5.0)
        assert s.interval_seconds == 5.0

    def test_system_interval_must_be_positive(self) -> None:
        with pytest.raises(Exception):
            SystemInstrumentation(interval_seconds=0)

    def test_process_defaults(self) -> None:
        p = ProcessInstrumentation()
        assert p.type == "process"
        assert p.enabled is True
        assert p.interval_seconds == 15.0
        assert p.metrics.cpu is True
        assert p.metrics.memory is False

    def test_apx_defaults(self) -> None:
        a = ApxInstrumentation()
        assert a.type == "apx"
        assert a.enabled is True
        assert a.metrics.dispatch_total is False

    def test_discriminated_union_from_dict(self) -> None:
        """Configuration parses typed dicts via the discriminated union."""
        config = Configuration(
            instrumentations=[
                {"type": "http", "enabled": False},
                {"type": "system", "metrics": {"swap": True}},
                {"type": "process", "metrics": {"threads": True}},
                {"type": "apx", "metrics": {"dispatch_total": True}},
            ]
        )
        types = [i.type for i in config.instrumentations]
        assert types == ["http", "system", "process", "apx"]

        http = config.instrumentations[0]
        assert isinstance(http, HttpInstrumentation)
        assert http.enabled is False

        system = config.instrumentations[1]
        assert isinstance(system, SystemInstrumentation)
        assert system.metrics.swap is True
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
                    SystemInstrumentation(metrics=SystemMetrics(cpu=False, swap=True))
                ]
            )
        )
        config = _get_config()
        system = next(i for i in config["instrumentations"] if i["type"] == "system")
        assert system["metrics"]["cpu"] is False
        assert system["metrics"]["swap"] is True
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
                        interval_seconds=5.0,
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
        assert process["interval_seconds"] == 5.0
        assert process["metrics"]["memory"] is True
        assert process["metrics"]["cpu"] is True


# ── metric_catalog() introspection ────────────────────────────────────────


EXPECTED_GROUPS = {"system", "process", "http", "apx"}
EXPECTED_SCOPES = {"supervisor", "worker", "both"}

EXPECTED_SYSTEM_METRICS = {
    "system.cpu.simple_utilization",
    "system.memory.utilization",
    "system.swap.utilization",
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
}


class TestMetricCatalog:
    def test_returns_list(self) -> None:
        catalog = metric_catalog()
        assert isinstance(catalog, list)

    def test_count(self) -> None:
        catalog = metric_catalog()
        assert len(catalog) == 16

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
