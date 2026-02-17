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
    pub timestamp_ns: i64,
    pub observed_timestamp_ns: i64,
    pub severity_number: Option<i32>,
    pub severity_text: Option<String>,
    pub body: Option<String>,
    pub service_name: Option<String>,
    pub app_path: Option<String>,
    pub resource_attributes: Option<String>, // JSON
    pub log_attributes: Option<String>,      // JSON
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl LogRecord {
    /// Return the effective timestamp in milliseconds, falling back to
    /// `observed_timestamp_ns` when `timestamp_ns` is zero (e.g. OpenTelemetry
    /// tracing bridge logs).
    pub fn effective_timestamp_ms(&self) -> i64 {
        let ns = if self.timestamp_ns == 0 {
            self.observed_timestamp_ns
        } else {
            self.timestamp_ns
        };
        ns / 1_000_000
    }

    /// Derive a short source label from `service_name`.
    pub fn source_label(&self) -> &'static str {
        source_label(self.service_name.as_deref().unwrap_or("unknown"))
    }
}

/// Derive a short source label from a service name string.
pub fn source_label(service_name: &str) -> &'static str {
    if service_name.ends_with("_app") {
        "app"
    } else if service_name.ends_with("_ui") {
        "ui"
    } else if service_name.ends_with("_db") {
        "db"
    } else {
        "apx"
    }
}

// ---------------------------------------------------------------------------
// Log aggregation
// ---------------------------------------------------------------------------

/// Time window for aggregating similar messages (in milliseconds).
const AGGREGATION_WINDOW_MS: i64 = 2000;

/// Minimum severity level for apx internal logs (DEBUG = 5, skipping TRACE = 1-4).
const APX_MIN_SEVERITY: i32 = 5;

/// Check if a log record should be skipped (internal/noisy logs).
pub fn should_skip_log(record: &LogRecord) -> bool {
    let message = record.body.as_deref().unwrap_or("");
    let service_name = record.service_name.as_deref().unwrap_or("");
    let severity_number = record.severity_number.unwrap_or(9);

    if service_name == "_core" && severity_number < APX_MIN_SEVERITY {
        return true;
    }

    if message.starts_with("BatchLogProcessor.")
        || message.starts_with("ReqwestBlockingClient.")
        || message.starts_with("HttpLogsClient.")
        || message.starts_with("HttpClient.")
        || message.starts_with("Http::connect")
    {
        return true;
    }

    if message.starts_with("starting new connection:")
        || message.starts_with("connecting to ")
        || message.starts_with("connected to ")
        || message.starts_with("reuse idle connection")
        || message.starts_with("pooling idle connection")
    {
        return true;
    }

    if message.starts_with("preparing query ")
        || message.starts_with("DEBUG: parse ")
        || message.starts_with("DEBUG: bind ")
        || message.starts_with("executing statement ")
    {
        return true;
    }

    if message.starts_with("take? (")
        || message.starts_with("wait at most")
        || message.starts_with("connection ")
        || message.contains(".cargo/registry/src/")
        || message.starts_with("event /")
    {
        return true;
    }

    false
}

/// Get aggregation key for a message if it should be aggregated.
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
    pub count: usize,
    pub timestamp_ms: i64,
    pub template: &'static str,
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

/// Get the flux directory path (~/.apx/logs).
pub fn flux_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(FLUX_DIR))
}
