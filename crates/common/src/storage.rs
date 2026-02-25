//! Pure types and logic for flux OTEL logs.
//!
//! This module contains log record types, filtering, and aggregation logic.
//! Database operations have been moved to the `apx-db` crate.

use std::collections::HashMap;
use std::path::PathBuf;

/// Directory for flux data (~/.apx/logs)
const FLUX_DIR: &str = ".apx/logs";

/// A log record to be inserted into the database.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Event timestamp in nanoseconds since epoch.
    pub timestamp_ns: i64,
    /// Observed timestamp in nanoseconds since epoch.
    pub observed_timestamp_ns: i64,
    /// OTLP severity number (1=TRACE, 9=INFO, 17=ERROR, etc.).
    pub severity_number: Option<i32>,
    /// Human-readable severity level (e.g. "INFO", "ERROR").
    pub severity_text: Option<String>,
    /// Log message body.
    pub body: Option<String>,
    /// Service name that emitted this log (e.g. "myapp_app", "myapp_db").
    pub service_name: Option<String>,
    /// Filesystem path of the originating application.
    pub app_path: Option<String>,
    /// OTLP resource attributes serialized as JSON.
    pub resource_attributes: Option<String>,
    /// OTLP log attributes serialized as JSON.
    pub log_attributes: Option<String>,
    /// Distributed trace identifier.
    pub trace_id: Option<String>,
    /// Span identifier within a trace.
    pub span_id: Option<String>,
}

impl LogRecord {
    /// Return the effective timestamp in milliseconds, falling back to
    /// `observed_timestamp_ns` when `timestamp_ns` is zero (e.g. OpenTelemetry
    /// tracing bridge logs).
    #[must_use]
    pub const fn effective_timestamp_ms(&self) -> i64 {
        let ns = if self.timestamp_ns == 0 {
            self.observed_timestamp_ns
        } else {
            self.timestamp_ns
        };
        ns / 1_000_000
    }

    /// Derive a short source label from `service_name`.
    #[must_use]
    pub fn source_label(&self) -> &'static str {
        ServiceKind::from_service_name(self.service_name.as_deref().unwrap_or("unknown")).label()
    }
}

/// Fixed set of service kinds for display and color-coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// Backend application service (`_app` suffix).
    App,
    /// Frontend UI service (`_ui` suffix).
    Ui,
    /// Database proxy service (`_db` suffix).
    Db,
    /// Any other service.
    Other,
}

impl ServiceKind {
    /// Classify a service name by its `_app` / `_ui` / `_db` suffix.
    #[must_use]
    pub fn from_service_name(name: &str) -> Self {
        if name.ends_with("_app") {
            Self::App
        } else if name.ends_with("_ui") {
            Self::Ui
        } else if name.ends_with("_db") {
            Self::Db
        } else {
            Self::Other
        }
    }

    /// Short display label: `"app"`, `"ui"`, `"db"`, or `"apx"`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Ui => "ui",
            Self::Db => "db",
            Self::Other => "apx",
        }
    }
}

/// Derive a short source label from a service name string.
#[must_use]
pub fn source_label(service_name: &str) -> &'static str {
    ServiceKind::from_service_name(service_name).label()
}

// ---------------------------------------------------------------------------
// Log aggregation
// ---------------------------------------------------------------------------

/// Time window for aggregating similar messages (in milliseconds).
const AGGREGATION_WINDOW_MS: i64 = 2000;

/// Minimum severity level for apx internal logs (DEBUG = 5, skipping TRACE = 1-4).
const APX_MIN_SEVERITY: i32 = 5;

/// Check if a log message (raw string) should be skipped.
///
/// This is used by OTEL forwarding in `process.rs` where only the message string
/// is available. The full `should_skip_log(&LogRecord)` delegates to this function
/// for message-based filtering.
#[must_use]
pub fn should_skip_log_message(message: &str) -> bool {
    // OTEL batch processor internals
    if message.starts_with("BatchLogProcessor.")
        || message.starts_with("ReqwestBlockingClient.")
        || message.starts_with("HttpLogsClient.")
        || message.starts_with("HttpClient.")
        || message.starts_with("Http::connect")
    {
        return true;
    }

    // HTTP connection pooling logs (hyper/reqwest)
    if message.starts_with("starting new connection:")
        || message.starts_with("connecting to ")
        || message.starts_with("connected to ")
        || message.starts_with("reuse idle connection")
        || message.starts_with("pooling idle connection")
    {
        return true;
    }

    // Tokio-postgres internal debug logs
    if message.starts_with("preparing query ")
        || message.starts_with("DEBUG: parse ")
        || message.starts_with("DEBUG: bind ")
        || message.starts_with("executing statement ")
    {
        return true;
    }

    // Connection pool noise
    if message.starts_with("take? (")
        || message.starts_with("wait at most")
        || message.starts_with("connection ")
        || message.contains(".cargo/registry/src/")
        || message.starts_with("event /")
    {
        return true;
    }

    // Sensitive data patterns (may contain passwords)
    if message.contains("WITH PASSWORD") || message.contains("PASSWORD '") {
        return true;
    }

    false
}

/// Check if a log record should be skipped (internal/noisy logs).
#[must_use]
pub fn should_skip_log(record: &LogRecord) -> bool {
    let service_name = record.service_name.as_deref().unwrap_or("");
    let severity_number = record.severity_number.unwrap_or(9);

    // Filter low-severity _core service logs (TRACE level)
    if service_name == "_core" && severity_number < APX_MIN_SEVERITY {
        return true;
    }

    let message = record.body.as_deref().unwrap_or("");
    should_skip_log_message(message)
}

/// Get aggregation key for a message if it should be aggregated.
#[must_use]
pub fn get_aggregation_key(record: &LogRecord) -> Option<(String, &'static str)> {
    let message = record.body.as_deref().unwrap_or("");
    let service = record.service_name.as_deref().unwrap_or("");

    if service.ends_with("_db") && message.starts_with("Client connected from") {
        return Some((
            format!("{service}_client_connected"),
            "db connections in last 2s",
        ));
    }

    if service.ends_with("_db") && message.starts_with("Client disconnected") {
        return Some((
            format!("{service}_client_disconnected"),
            "db disconnections in last 2s",
        ));
    }

    None
}

/// A single flushed aggregation bucket.
#[derive(Debug, Clone)]
pub struct AggregatedRecord {
    /// Number of messages aggregated in this bucket.
    pub count: usize,
    /// Timestamp (ms) of the first message in the bucket.
    pub timestamp_ms: i64,
    /// Human-readable summary template for the aggregated messages.
    pub template: &'static str,
    /// Service name that produced the aggregated messages.
    pub service_name: String,
}

/// Internal bucket used by [`LogAggregator`].
#[derive(Debug)]
struct AggBucket {
    count: usize,
    first_ts_ms: i64,
    last_ts_ms: i64,
    template: &'static str,
    service_name: String,
}

/// Tracks aggregated messages within time windows.
#[derive(Debug, Default)]
pub struct LogAggregator {
    buckets: HashMap<String, AggBucket>,
}

// Manual Debug for AggBucket is not needed since LogAggregator uses a custom Debug
// that only shows bucket count.

impl LogAggregator {
    /// Create a new empty aggregator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to aggregate the record. Returns `true` if it was aggregated.
    pub fn add(&mut self, record: &LogRecord) -> bool {
        let Some((key, template)) = get_aggregation_key(record) else {
            return false;
        };

        let timestamp_ms = record.effective_timestamp_ms();
        let service_name = record.service_name.as_deref().unwrap_or("").to_string();

        let entry = self.buckets.entry(key).or_insert(AggBucket {
            count: 0,
            first_ts_ms: timestamp_ms,
            last_ts_ms: timestamp_ms,
            template,
            service_name,
        });
        entry.count += 1;
        entry.last_ts_ms = timestamp_ms;

        true
    }

    /// Flush buckets whose last timestamp is older than `current_time_ms` by
    /// more than the aggregation window. Only returns buckets with count > 1.
    pub fn flush_expired(&mut self, current_time_ms: i64) -> Vec<AggregatedRecord> {
        let mut output = Vec::new();
        let mut to_remove = Vec::new();

        for (key, bucket) in &self.buckets {
            if current_time_ms - bucket.last_ts_ms > AGGREGATION_WINDOW_MS {
                if bucket.count > 1 {
                    output.push(AggregatedRecord {
                        count: bucket.count,
                        timestamp_ms: bucket.first_ts_ms,
                        template: bucket.template,
                        service_name: bucket.service_name.clone(),
                    });
                }
                to_remove.push(key.clone());
            }
        }

        for key in to_remove {
            self.buckets.remove(&key);
        }

        output
    }

    /// Flush all remaining buckets. Only returns buckets with count > 1.
    pub fn flush_all(&mut self) -> Vec<AggregatedRecord> {
        let mut output = Vec::new();

        for bucket in self.buckets.values() {
            if bucket.count > 1 {
                output.push(AggregatedRecord {
                    count: bucket.count,
                    timestamp_ms: bucket.first_ts_ms,
                    template: bucket.template,
                    service_name: bucket.service_name.clone(),
                });
            }
        }

        self.buckets.clear();
        output
    }
}

/// Get the flux directory path (`~/.apx/logs`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn flux_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(FLUX_DIR))
}
