//! Telemetry configuration read from the Python `apx.telemetry` module.
//!
//! The Python side defines a `Configuration` Pydantic model with a list of
//! typed instrumentations. This module reads the effective config (defaults
//! merged with user overrides) and flattens it into Rust structs for zero-cost
//! runtime access.

use std::collections::HashSet;

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
}

/// System metrics instrumentation configuration.
#[derive(Debug, Clone)]
pub struct SystemConfig {
    /// Whether system metrics collection is enabled.
    pub enabled: bool,
    /// Which metrics to collect.
    pub metrics: HashSet<SystemMetricKind>,
    /// Collection interval in seconds.
    pub interval_secs: f64,
}

/// Available system metric kinds, matching OTEL semantic conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemMetricKind {
    /// `process.cpu.utilization`
    ProcessCpu,
    /// `system.cpu.simple_utilization`
    SystemCpu,
    /// `system.memory.utilization`
    SystemMemory,
    /// `system.swap.utilization`
    SystemSwap,
    /// `process.memory.usage`
    ProcessMemory,
    /// `process.thread.count`
    ProcessThreads,
    /// `system.disk.io`
    SystemDiskIo,
    /// `system.network.io`
    SystemNetworkIo,
}

/// HTTP transport instrumentation configuration.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Whether HTTP header capture is enabled.
    pub enabled: bool,
    /// Request header names to capture as span attributes.
    pub capture_request_headers: Vec<String>,
    /// Response header names to capture as span attributes.
    pub capture_response_headers: Vec<String>,
    /// Header name patterns whose values are replaced with `[REDACTED]`.
    pub sanitize_headers: Vec<String>,
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

// ── Python config reading ────────────────────────────────────────────────

/// Read telemetry configuration from the Python `apx.telemetry` module.
///
/// Calls `apx.telemetry._get_config()` which returns the merged effective
/// configuration (defaults + user overrides) as a dict.
pub fn read_python_config(py: Python<'_>) -> PyResult<TelemetryConfig> {
    let module = py.import(c"apx.telemetry")?;
    let get_config = module.getattr(c"_get_config")?;
    let config_obj = get_config.call0()?;
    let config_dict: &Bound<'_, PyDict> = config_obj.cast()?;

    let instrumentations_obj = config_dict
        .get_item("instrumentations")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("instrumentations"))?;
    let instrumentations: &Bound<'_, PyList> = instrumentations_obj.cast()?;

    let mut system = SystemConfig {
        enabled: true,
        metrics: default_system_metrics(),
        interval_secs: 15.0,
    };
    let mut http = HttpConfig {
        enabled: true,
        capture_request_headers: Vec::new(),
        capture_response_headers: Vec::new(),
        sanitize_headers: Vec::new(),
    };
    let mut fastapi = FastApiConfig {
        enabled: true,
        excluded_routes: Vec::new(),
        record_route: true,
    };

    for item in instrumentations.iter() {
        let dict: &Bound<'_, PyDict> = item.cast()?;
        let type_str: String = extract_string(dict, "type")?;
        match type_str.as_str() {
            "system" => system = parse_system_config(dict)?,
            "http" => http = parse_http_config(dict)?,
            "fastapi" => fastapi = parse_fastapi_config(dict)?,
            _ => {
                tracing::debug!(instrumentation_type = %type_str, "unknown instrumentation type, skipping");
            }
        }
    }

    Ok(TelemetryConfig {
        system,
        http,
        fastapi,
    })
}

// ── Parsing helpers ──────────────────────────────────────────────────────

fn default_system_metrics() -> HashSet<SystemMetricKind> {
    HashSet::from([
        SystemMetricKind::ProcessCpu,
        SystemMetricKind::SystemCpu,
        SystemMetricKind::SystemMemory,
    ])
}

fn parse_system_config(dict: &Bound<'_, PyDict>) -> PyResult<SystemConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let interval_secs = extract_float(dict, "interval_seconds", 15.0)?;

    let mut metrics = HashSet::new();
    if let Some(collect) = dict.get_item("collect")? {
        for item in collect.try_iter()? {
            let value: String = item?.extract()?;
            if let Some(kind) = parse_metric_kind(&value) {
                metrics.insert(kind);
            }
        }
    }
    if metrics.is_empty() {
        metrics = default_system_metrics();
    }

    Ok(SystemConfig {
        enabled,
        metrics,
        interval_secs,
    })
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

    Ok(HttpConfig {
        enabled,
        capture_request_headers: req_headers,
        capture_response_headers: resp_headers,
        sanitize_headers: sanitize,
    })
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

fn parse_metric_kind(value: &str) -> Option<SystemMetricKind> {
    match value {
        "process.cpu.utilization" => Some(SystemMetricKind::ProcessCpu),
        "system.cpu.simple_utilization" => Some(SystemMetricKind::SystemCpu),
        "system.memory.utilization" => Some(SystemMetricKind::SystemMemory),
        "system.swap.utilization" => Some(SystemMetricKind::SystemSwap),
        "process.memory.usage" => Some(SystemMetricKind::ProcessMemory),
        "process.thread.count" => Some(SystemMetricKind::ProcessThreads),
        "system.disk.io" => Some(SystemMetricKind::SystemDiskIo),
        "system.network.io" => Some(SystemMetricKind::SystemNetworkIo),
        _ => None,
    }
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
