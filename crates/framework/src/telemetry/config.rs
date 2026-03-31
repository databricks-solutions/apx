//! Telemetry configuration read from the Python `apx.telemetry` module.
//!
//! The Python side defines a `Configuration` Pydantic model with a list of
//! typed instrumentations. This module reads the effective config (defaults
//! merged with user overrides) and flattens it into Rust structs for zero-cost
//! runtime access.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde::{Deserialize, Serialize};

// ── Domain types ─────────────────────────────────────────────────────────

/// User-defined OTLP resource attributes from Python `Configuration.resource`.
#[derive(Debug, Clone, Default)]
pub struct ResourceConfig {
    /// User-provided key-value pairs merged into `resource.attributes`.
    pub attributes: Vec<(String, String)>,
    /// Optional override for the resource schema URL (rare).
    pub schema_url: Option<String>,
}

/// Top-level telemetry configuration, flattened from the Python model.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// User-defined OTLP resource attributes.
    pub resource: ResourceConfig,
    /// Machine-wide system metrics (supervisor only).
    pub system: SystemConfig,
    /// Per-process metrics (each worker + supervisor).
    pub process: ProcessConfig,
    /// Transport-level HTTP instrumentation.
    pub http: HttpConfig,
    /// APX framework dispatch timing metrics.
    pub apx: ApxConfig,
}

/// System-global metrics instrumentation configuration (supervisor only).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SystemConfig {
    /// Whether system metrics collection is enabled.
    pub enabled: bool,
    /// Per-metric enable flags for machine-wide gauges.
    pub metrics: SystemGlobalToggles,
}

/// Per-metric boolean toggles for system-global instrumentation.
///
/// These metrics are collected once on the supervisor process.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one bool per OTEL metric toggle"
)]
pub struct SystemGlobalToggles {
    /// Toggle for [`super::defs::SYSTEM_CPU`].
    pub system_cpu: bool,
    /// Toggle for [`super::defs::SYSTEM_MEMORY`].
    pub system_memory: bool,
    /// Toggle for [`super::defs::SYSTEM_PAGING`].
    pub system_paging: bool,
    /// Toggle for [`super::defs::SYSTEM_DISK_IO`].
    pub system_disk_io: bool,
    /// Toggle for [`super::defs::SYSTEM_NETWORK_IO`].
    pub system_network_io: bool,
}

impl Default for SystemGlobalToggles {
    fn default() -> Self {
        Self {
            system_cpu: true,
            system_memory: true,
            system_paging: false,
            system_disk_io: false,
            system_network_io: false,
        }
    }
}

/// Per-process metrics instrumentation configuration (each worker + supervisor).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProcessConfig {
    /// Whether process metrics collection is enabled.
    pub enabled: bool,
    /// Per-metric enable flags for process-level gauges.
    pub metrics: ProcessMetricToggles,
}

/// Per-metric boolean toggles for process-level instrumentation.
///
/// These metrics are collected per-worker and once on the supervisor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProcessMetricToggles {
    /// Toggle for [`super::defs::PROCESS_CPU`].
    pub process_cpu: bool,
    /// Toggle for [`super::defs::PROCESS_MEMORY`].
    pub process_memory: bool,
    /// Toggle for [`super::defs::PROCESS_THREADS`].
    pub process_threads: bool,
}

impl Default for ProcessMetricToggles {
    fn default() -> Self {
        Self {
            process_cpu: true,
            process_memory: false,
            process_threads: false,
        }
    }
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
    /// Per-metric enable flags.
    pub metrics: HttpMetricToggles,
}

/// Per-metric boolean toggles for HTTP instrumentation.
#[derive(Debug, Clone, Copy)]
pub struct HttpMetricToggles {
    /// Toggle for [`super::defs::HTTP_REQUEST_DURATION`].
    pub server_request_duration: bool,
    /// Toggle for [`super::defs::HTTP_ACTIVE_REQUESTS`].
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

/// APX framework dispatch timing instrumentation configuration.
#[derive(Debug, Clone, Copy)]
pub struct ApxConfig {
    /// Whether APX dispatch metrics are enabled.
    pub enabled: bool,
    /// Per-metric enable flags.
    pub metrics: ApxMetricToggles,
}

/// Per-metric boolean toggles for APX dispatch timing.
#[derive(Debug, Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one bool per OTEL metric toggle"
)]
pub struct ApxMetricToggles {
    /// Toggle for [`super::defs::DISPATCH_BODY_COLLECT`].
    pub dispatch_body_collect: bool,
    /// Toggle for [`super::defs::DISPATCH_CROSSBEAM_SEND`].
    pub dispatch_crossbeam_send: bool,
    /// Toggle for [`super::defs::DISPATCH_RESPONSE_WAIT`].
    pub dispatch_response_wait: bool,
    /// Toggle for [`super::defs::DISPATCH_TOTAL`].
    pub dispatch_total: bool,
    /// Toggle for [`super::defs::ASGI_RECEIVE_BUILD`].
    pub asgi_receive_build: bool,
    /// Toggle for [`super::defs::ASGI_SEND_PARSE`].
    pub asgi_send_parse: bool,
    /// Toggle for [`super::defs::DISPATCH_PICKUP_DELAY`].
    pub dispatch_pickup_delay: bool,
    /// Toggle for [`super::defs::DISPATCH_MATERIALIZE`].
    pub dispatch_materialize: bool,
    /// Toggle for [`super::defs::DISPATCH_QUEUE_DEPTH`].
    pub dispatch_queue_depth: bool,
}

// ── Public defaults (used by supervisor) ─────────────────────────────────

/// Default system-global config matching Python defaults.
pub fn default_system_config() -> SystemConfig {
    SystemConfig {
        enabled: true,
        metrics: SystemGlobalToggles::default(),
    }
}

/// Default per-process config matching Python defaults.
pub fn default_process_config() -> ProcessConfig {
    ProcessConfig {
        enabled: true,
        metrics: ProcessMetricToggles::default(),
    }
}

// ── Python config reading ────────────────────────────────────────────────

/// Read telemetry configuration from the Python `apx.telemetry` module.
///
/// Calls `apx.telemetry._get_config()` which returns the merged effective
/// configuration (defaults + user overrides) as a dict.
pub fn read_python_config(py: Python<'_>) -> PyResult<TelemetryConfig> {
    tracing::trace!(name: "apx.telemetry.config.read_start", target: "apx::telemetry", "reading telemetry config from apx.telemetry._get_config()");

    let module = py.import(c"apx.telemetry")?;
    let get_config = module.getattr(c"_get_config")?;
    let config_obj = get_config.call0()?;
    let config_dict: &Bound<'_, PyDict> = config_obj.cast()?;

    let instrumentations_obj = config_dict
        .get_item("instrumentations")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("instrumentations"))?;
    let instrumentations: &Bound<'_, PyList> = instrumentations_obj.cast()?;

    tracing::trace!(
        name: "apx.telemetry.config.instrumentations_found",
        target: "apx::telemetry",
        count = instrumentations.len(),
        "instrumentations found in Python telemetry config"
    );

    let resource = if let Some(resource_obj) = config_dict.get_item("resource")? {
        parse_resource_config(resource_obj.cast()?)?
    } else {
        ResourceConfig::default()
    };

    let mut system = default_system_config();
    let mut process = default_process_config();
    let mut http = default_http_config();
    let mut apx = default_apx_config();

    for item in instrumentations.iter() {
        let dict: &Bound<'_, PyDict> = item.cast()?;
        let type_str: String = extract_string(dict, "type")?;
        match type_str.as_str() {
            "system" => {
                system = parse_system_config(dict)?;
                tracing::trace!(
                    name: "apx.telemetry.config.parsed_system",
                    target: "apx::telemetry",
                    enabled = system.enabled,
                    "parsed system instrumentation config"
                );
            }
            "process" => {
                process = parse_process_config(dict)?;
                tracing::trace!(
                    name: "apx.telemetry.config.parsed_process",
                    target: "apx::telemetry",
                    enabled = process.enabled,
                    "parsed process instrumentation config"
                );
            }
            "http" => {
                http = parse_http_config(dict)?;
                tracing::trace!(
                    name: "apx.telemetry.config.parsed_http",
                    target: "apx::telemetry",
                    enabled = http.enabled,
                    "parsed http instrumentation config"
                );
            }
            "apx" => {
                apx = parse_apx_config(dict)?;
                tracing::trace!(
                    name: "apx.telemetry.config.parsed_apx",
                    target: "apx::telemetry",
                    enabled = apx.enabled,
                    "parsed apx instrumentation config"
                );
            }
            _ => {
                tracing::debug!(
                    name: "apx.telemetry.config.unknown_instrumentation_skipped",
                    target: "apx::telemetry",
                    instrumentation_type = %type_str,
                    "unknown instrumentation type, skipping"
                );
            }
        }
    }

    tracing::trace!(
        name: "apx.telemetry.config.resolved",
        target: "apx::telemetry",
        system_enabled = system.enabled,
        process_enabled = process.enabled,
        http_enabled = http.enabled,
        apx_enabled = apx.enabled,
        "telemetry config resolved"
    );

    Ok(TelemetryConfig {
        resource,
        system,
        process,
        http,
        apx,
    })
}

// ── Private defaults ─────────────────────────────────────────────────────

fn default_http_config() -> HttpConfig {
    HttpConfig {
        enabled: true,
        capture_request_headers: Vec::new(),
        capture_response_headers: Vec::new(),
        sanitize_headers: Vec::new(),
        metrics: HttpMetricToggles::default(),
    }
}

fn default_apx_config() -> ApxConfig {
    ApxConfig {
        enabled: true,
        metrics: ApxMetricToggles::default(),
    }
}

// ── Parsing helpers ──────────────────────────────────────────────────────

fn parse_resource_config(dict: &Bound<'_, PyDict>) -> PyResult<ResourceConfig> {
    let mut attributes = Vec::new();
    if let Some(attrs_obj) = dict.get_item("attributes")? {
        let attrs_list: &Bound<'_, PyList> = attrs_obj.cast()?;
        for item in attrs_list.iter() {
            let attr_dict: &Bound<'_, PyDict> = item.cast()?;
            let key: String = extract_string(attr_dict, "key")?;
            let value: String = extract_string(attr_dict, "value")?;
            attributes.push((key, value));
        }
    }
    let schema_url = dict
        .get_item("schema_url")?
        .map(|v| v.extract())
        .transpose()?;

    Ok(ResourceConfig {
        attributes,
        schema_url,
    })
}

fn parse_system_config(dict: &Bound<'_, PyDict>) -> PyResult<SystemConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let metrics = if let Some(metrics_dict) = dict.get_item("metrics")? {
        parse_system_global_toggles(metrics_dict.cast()?)
    } else {
        SystemGlobalToggles::default()
    };

    Ok(SystemConfig { enabled, metrics })
}

fn parse_system_global_toggles(dict: &Bound<'_, PyDict>) -> SystemGlobalToggles {
    let defaults = SystemGlobalToggles::default();
    SystemGlobalToggles {
        system_cpu: extract_bool_or(dict, "cpu", defaults.system_cpu),
        system_memory: extract_bool_or(dict, "memory", defaults.system_memory),
        system_paging: extract_bool_or(dict, "paging", defaults.system_paging),
        system_disk_io: extract_bool_or(dict, "disk_io", defaults.system_disk_io),
        system_network_io: extract_bool_or(dict, "network_io", defaults.system_network_io),
    }
}

fn parse_process_config(dict: &Bound<'_, PyDict>) -> PyResult<ProcessConfig> {
    let enabled = extract_bool(dict, "enabled", true)?;
    let metrics = if let Some(metrics_dict) = dict.get_item("metrics")? {
        parse_process_metric_toggles(metrics_dict.cast()?)
    } else {
        ProcessMetricToggles::default()
    };

    Ok(ProcessConfig { enabled, metrics })
}

fn parse_process_metric_toggles(dict: &Bound<'_, PyDict>) -> ProcessMetricToggles {
    let defaults = ProcessMetricToggles::default();
    ProcessMetricToggles {
        process_cpu: extract_bool_or(dict, "cpu", defaults.process_cpu),
        process_memory: extract_bool_or(dict, "memory", defaults.process_memory),
        process_threads: extract_bool_or(dict, "threads", defaults.process_threads),
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
    let defaults = HttpMetricToggles::default();
    HttpMetricToggles {
        server_request_duration: extract_bool_or(
            dict,
            "server_request_duration",
            defaults.server_request_duration,
        ),
        server_active_requests: extract_bool_or(
            dict,
            "server_active_requests",
            defaults.server_active_requests,
        ),
    }
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
    ApxMetricToggles {
        dispatch_body_collect: extract_bool_or(dict, "dispatch_body_collect", false),
        dispatch_crossbeam_send: extract_bool_or(dict, "dispatch_crossbeam_send", false),
        dispatch_response_wait: extract_bool_or(dict, "dispatch_response_wait", false),
        dispatch_total: extract_bool_or(dict, "dispatch_total", false),
        asgi_receive_build: extract_bool_or(dict, "asgi_receive_build", false),
        asgi_send_parse: extract_bool_or(dict, "asgi_send_parse", false),
        dispatch_pickup_delay: extract_bool_or(dict, "dispatch_pickup_delay", false),
        dispatch_materialize: extract_bool_or(dict, "dispatch_materialize", false),
        dispatch_queue_depth: extract_bool_or(dict, "dispatch_queue_depth", false),
    }
}

// ── Low-level extractors ─────────────────────────────────────────────────

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

/// Extract a bool from a dict, returning `default` on any error.
fn extract_bool_or(dict: &Bound<'_, PyDict>, key: &str, default: bool) -> bool {
    extract_bool(dict, key, default).unwrap_or(default)
}

fn extract_string_list(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<String>> {
    let Some(val) = dict.get_item(key)? else {
        return Ok(Vec::new());
    };
    let list: &Bound<'_, PyList> = val.cast()?;
    list.iter().map(|item| item.extract()).collect()
}
