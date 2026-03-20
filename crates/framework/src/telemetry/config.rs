//! Telemetry configuration read from the Python `apx.telemetry` module.
//!
//! The Python side defines a `Configuration` Pydantic model with a list of
//! typed instrumentations. This module reads the effective config (defaults
//! merged with user overrides) and flattens it into Rust structs for zero-cost
//! runtime access.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

// ── Domain types ─────────────────────────────────────────────────────────

/// Top-level telemetry configuration, flattened from the Python model.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// System metrics collection.
    pub system: SystemConfig,
    /// Transport-level HTTP instrumentation.
    pub http: HttpConfig,
    /// FastAPI/Starlette framework instrumentation.
    pub fastapi: FastApiConfig,
    /// APX framework dispatch timing metrics.
    pub apx: ApxConfig,
}

/// System metrics instrumentation configuration.
#[derive(Debug, Clone, Copy)]
pub struct SystemConfig {
    /// Whether system metrics collection is enabled.
    pub enabled: bool,
    /// Collection interval in seconds.
    pub interval_secs: f64,
    /// Per-metric enable flags mirroring `SystemMetrics` in Python.
    pub metrics: SystemMetricToggles,
}

/// Per-metric boolean toggles for system instrumentation.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SystemMetricToggles {
    /// Enable `process.cpu.utilization` gauge.
    pub process_cpu: bool,
    /// Enable `system.cpu.simple_utilization` gauge.
    pub system_cpu: bool,
    /// Enable `system.memory.utilization` gauge.
    pub system_memory: bool,
    /// Enable `system.swap.utilization` gauge.
    pub system_swap: bool,
    /// Enable `process.memory.usage` gauge.
    pub process_memory: bool,
    /// Enable `process.thread.count` gauge.
    pub process_threads: bool,
    /// Enable `system.disk.io` gauge.
    pub system_disk_io: bool,
    /// Enable `system.network.io` gauge.
    pub system_network_io: bool,
}

/// HTTP transport instrumentation configuration.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Whether HTTP instrumentation is enabled.
    pub enabled: bool,
    /// Request header names to capture as span attributes.
    pub capture_request_headers: Vec<String>,
    /// Response header names to capture as span attributes.
    pub capture_response_headers: Vec<String>,
    /// Header name patterns whose values are replaced with `[REDACTED]`.
    pub sanitize_headers: Vec<String>,
    /// Per-metric enable flags mirroring `HttpMetrics` in Python.
    pub metrics: HttpMetricToggles,
}

/// Per-metric boolean toggles for HTTP instrumentation.
#[derive(Debug, Clone, Copy)]
pub struct HttpMetricToggles {
    /// Enable `http.server.request.duration` histogram.
    pub server_request_duration: bool,
    /// Enable `http.server.active_requests` up-down counter.
    pub server_active_requests: bool,
}

impl Default for HttpMetricToggles {
    fn default() -> Self {
        Self {
            server_request_duration: true,
            server_active_requests: true,
        }
    }
}

/// FastAPI/Starlette framework instrumentation configuration.
#[derive(Debug, Clone)]
pub struct FastApiConfig {
    /// Whether FastAPI instrumentation is enabled.
    pub enabled: bool,
    /// URL regex patterns to exclude from tracing.
    pub excluded_routes: Vec<String>,
    /// Extract matched route template from Starlette scope.
    pub record_route: bool,
}

/// APX framework dispatch timing instrumentation configuration.
#[derive(Debug, Clone, Copy)]
pub struct ApxConfig {
    /// Whether APX dispatch metrics are enabled.
    pub enabled: bool,
    /// Per-metric enable flags mirroring `ApxMetrics` in Python.
    pub metrics: ApxMetricToggles,
}

/// Per-metric boolean toggles for APX dispatch timing.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ApxMetricToggles {
    /// Enable `apx.dispatch.body_collect.duration` histogram.
    pub dispatch_body_collect: bool,
    /// Enable `apx.dispatch.crossbeam_send.duration` histogram.
    pub dispatch_crossbeam_send: bool,
    /// Enable `apx.dispatch.response_wait.duration` histogram.
    pub dispatch_response_wait: bool,
    /// Enable `apx.dispatch.total.duration` histogram.
    pub dispatch_total: bool,
    /// Enable `apx.asgi.receive_build.duration` histogram.
    pub asgi_receive_build: bool,
    /// Enable `apx.asgi.send_parse.duration` histogram.
    pub asgi_send_parse: bool,
}

// ── Python config reading ────────────────────────────────────────────────

/// Read telemetry configuration from the Python `apx.telemetry` module.
///
/// Calls `apx.telemetry._get_config()` which returns the merged effective
/// configuration (defaults + user overrides) as a dict.
pub fn read_python_config(py: Python<'_>) -> PyResult<TelemetryConfig> {
    tracing::trace!(target: "apx::telemetry", "reading telemetry config from apx.telemetry._get_config()");

    let module = py.import(c"apx.telemetry")?;
    let get_config = module.getattr(c"_get_config")?;
    let config_obj = get_config.call0()?;
    let config_dict: &Bound<'_, PyDict> = config_obj.cast()?;

    let instrumentations_obj = config_dict
        .get_item("instrumentations")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("instrumentations"))?;
    let instrumentations: &Bound<'_, PyList> = instrumentations_obj.cast()?;

    tracing::trace!(
        target: "apx::telemetry",
        count = instrumentations.len(),
        "instrumentations found in Python telemetry config"
    );

    let mut system = default_system_config();
    let mut http = default_http_config();
    let mut fastapi = default_fastapi_config();
    let mut apx = default_apx_config();

    for item in instrumentations.iter() {
        let dict: &Bound<'_, PyDict> = item.cast()?;
        let type_str: String = extract_string(dict, "type")?;
        match type_str.as_str() {
            "system" => {
                system = parse_system_config(dict)?;
                tracing::trace!(
                    target: "apx::telemetry",
                    enabled = system.enabled,
                    interval_secs = system.interval_secs,
                    "parsed system instrumentation config"
                );
            }
            "http" => {
                http = parse_http_config(dict)?;
                tracing::trace!(
                    target: "apx::telemetry",
                    enabled = http.enabled,
                    "parsed http instrumentation config"
                );
            }
            "fastapi" => {
                fastapi = parse_fastapi_config(dict)?;
                tracing::trace!(
                    target: "apx::telemetry",
                    enabled = fastapi.enabled,
                    record_route = fastapi.record_route,
                    "parsed fastapi instrumentation config"
                );
            }
            "apx" => {
                apx = parse_apx_config(dict)?;
                tracing::trace!(
                    target: "apx::telemetry",
                    enabled = apx.enabled,
                    "parsed apx instrumentation config"
                );
            }
            _ => {
                tracing::debug!(instrumentation_type = %type_str, "unknown instrumentation type, skipping");
            }
        }
    }

    tracing::trace!(
        target: "apx::telemetry",
        system_enabled = system.enabled,
        http_enabled = http.enabled,
        fastapi_enabled = fastapi.enabled,
        apx_enabled = apx.enabled,
        "telemetry config resolved"
    );

    Ok(TelemetryConfig {
        system,
        http,
        fastapi,
        apx,
    })
}

// ── Default configs ──────────────────────────────────────────────────────

fn default_system_config() -> SystemConfig {
    SystemConfig {
        enabled: true,
        interval_secs: 15.0,
        metrics: SystemMetricToggles {
            process_cpu: true,
            system_cpu: true,
            system_memory: true,
            ..Default::default()
        },
    }
}

fn default_http_config() -> HttpConfig {
    HttpConfig {
        enabled: true,
        capture_request_headers: Vec::new(),
        capture_response_headers: Vec::new(),
        sanitize_headers: Vec::new(),
        metrics: HttpMetricToggles::default(),
    }
}

fn default_fastapi_config() -> FastApiConfig {
    FastApiConfig {
        enabled: true,
        excluded_routes: Vec::new(),
        record_route: true,
    }
}

fn default_apx_config() -> ApxConfig {
    ApxConfig {
        enabled: true,
        metrics: ApxMetricToggles::default(),
    }
}

// ── Parsing helpers ──────────────────────────────────────────────────────

fn parse_system_config(dict: &Bound<'_, PyDict>) -> PyResult<SystemConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let interval_secs = extract_float(dict, "interval_seconds", 15.0)?;
    let metrics = if let Some(metrics_dict) = dict.get_item("metrics")? {
        parse_system_metric_toggles(metrics_dict.cast()?)
    } else {
        SystemMetricToggles {
            process_cpu: true,
            system_cpu: true,
            system_memory: true,
            ..Default::default()
        }
    };

    Ok(SystemConfig {
        enabled,
        interval_secs,
        metrics,
    })
}

fn parse_system_metric_toggles(dict: &Bound<'_, PyDict>) -> SystemMetricToggles {
    let b = |key: &str, default: bool| extract_metric_default(dict, key, default);
    SystemMetricToggles {
        process_cpu: b("process_cpu", true),
        system_cpu: b("system_cpu", true),
        system_memory: b("system_memory", true),
        system_swap: b("system_swap", false),
        process_memory: b("process_memory", false),
        process_threads: b("process_threads", false),
        system_disk_io: b("system_disk_io", false),
        system_network_io: b("system_network_io", false),
    }
}

fn parse_http_config(dict: &Bound<'_, PyDict>) -> PyResult<HttpConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;

    let (mut req_headers, mut resp_headers, mut sanitize) = (Vec::new(), Vec::new(), Vec::new());

    if let Some(capture) = dict.get_item("capture_headers")? {
        let capture_dict: &Bound<'_, PyDict> = capture.cast()?;
        req_headers = extract_string_list(capture_dict, "request")?;
        resp_headers = extract_string_list(capture_dict, "response")?;
        sanitize = extract_string_list(capture_dict, "sanitize")?;
    }

    let metrics = if let Some(metrics_dict) = dict.get_item("metrics")? {
        parse_http_metric_toggles(metrics_dict.cast()?)
    } else {
        HttpMetricToggles::default()
    };

    Ok(HttpConfig {
        enabled,
        capture_request_headers: req_headers,
        capture_response_headers: resp_headers,
        sanitize_headers: sanitize,
        metrics,
    })
}

fn parse_http_metric_toggles(dict: &Bound<'_, PyDict>) -> HttpMetricToggles {
    let b = |key: &str, default: bool| extract_metric_default(dict, key, default);
    HttpMetricToggles {
        server_request_duration: b("server_request_duration", true),
        server_active_requests: b("server_active_requests", true),
    }
}

fn parse_fastapi_config(dict: &Bound<'_, PyDict>) -> PyResult<FastApiConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let record_route = extract_bool(dict, "record_route", true)?;
    let excluded_routes = extract_string_list(dict, "excluded_routes")?;

    Ok(FastApiConfig {
        enabled,
        excluded_routes,
        record_route,
    })
}

fn parse_apx_config(dict: &Bound<'_, PyDict>) -> PyResult<ApxConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let metrics = if let Some(metrics_dict) = dict.get_item("metrics")? {
        parse_apx_metric_toggles(metrics_dict.cast()?)
    } else {
        ApxMetricToggles::default()
    };

    Ok(ApxConfig { enabled, metrics })
}

fn parse_apx_metric_toggles(dict: &Bound<'_, PyDict>) -> ApxMetricToggles {
    let b = |key: &str| extract_metric_default(dict, key, false);
    ApxMetricToggles {
        dispatch_body_collect: b("dispatch_body_collect"),
        dispatch_crossbeam_send: b("dispatch_crossbeam_send"),
        dispatch_response_wait: b("dispatch_response_wait"),
        dispatch_total: b("dispatch_total"),
        asgi_receive_build: b("asgi_receive_build"),
        asgi_send_parse: b("asgi_send_parse"),
    }
}

/// Extract the `default` boolean from a serialized `Metric` sub-dict.
///
/// The Python `Metric` model serialises as `{"title": "...", "description":
/// "...", "group": "...", "default": true/false}`. This helper dereferences
/// that nested structure and returns the `default` field, falling back to
/// `default` if the key is absent or the value is not a dict.
fn extract_metric_default(dict: &Bound<'_, PyDict>, key: &str, default: bool) -> bool {
    let Ok(Some(item)) = dict.get_item(key) else {
        return default;
    };
    let Ok(metric_dict) = item.cast::<PyDict>() else {
        return default;
    };
    extract_bool(metric_dict, "default", default).unwrap_or(default)
}

fn extract_string(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    dict.get_item(key)?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(key.to_owned()))?
        .extract()
}

fn extract_bool(dict: &Bound<'_, PyDict>, key: &str, default: bool) -> PyResult<bool> {
    dict.get_item(key)?
        .map(|v| v.extract())
        .transpose()
        .map(|v| v.unwrap_or(default))
}

fn extract_float(dict: &Bound<'_, PyDict>, key: &str, default: f64) -> PyResult<f64> {
    dict.get_item(key)?
        .map(|v| v.extract())
        .transpose()
        .map(|v| v.unwrap_or(default))
}

fn extract_string_list(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<String>> {
    let Some(val) = dict.get_item(key)? else {
        return Ok(Vec::new());
    };
    let list: &Bound<'_, PyList> = val.cast()?;
    list.iter().map(|item| item.extract()).collect()
}
