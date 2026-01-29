//! Log streaming for dev server startup.
//!
//! Prints real-time logs line-by-line during server startup.

use chrono::{Local, TimeZone, Utc};
use std::path::Path;

use crate::flux::{LogRecord, Storage};

/// Simple log streamer that prints logs line-by-line to stdout.
pub struct StartupLogStreamer {
    last_log_id: i64,
    storage: Option<Storage>,
    app_path: String,
}

impl StartupLogStreamer {
    /// Create a new log streamer for the given app directory.
    pub fn new(app_dir: &Path) -> Self {
        let app_path = app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf())
            .display()
            .to_string();

        let storage = Storage::open().ok();
        let last_log_id = storage
            .as_ref()
            .and_then(|s| s.get_latest_id().ok())
            .unwrap_or(0);

        Self {
            last_log_id,
            storage,
            app_path,
        }
    }

    /// Print any new logs since the last call.
    /// Returns the number of new log lines printed.
    pub fn print_new_logs(&mut self) -> usize {
        let storage = match &self.storage {
            Some(s) => s,
            None => return 0,
        };

        // Query logs since last ID
        let records = match storage.query_logs_after_id(Some(&self.app_path), self.last_log_id) {
            Ok(r) => r,
            Err(_) => return 0,
        };

        let mut count = 0;
        for record in &records {
            if !should_skip_log(record) {
                println!("{}", format_log_record(record));
                count += 1;
            }
        }

        // Update last_log_id
        if let Ok(new_id) = storage.get_latest_id() {
            if new_id > self.last_log_id {
                self.last_log_id = new_id;
            }
        }

        count
    }
}

/// Format a log record for terminal display (simplified version).
fn format_log_record(record: &LogRecord) -> String {
    let effective_timestamp_ns = if record.timestamp_ns == 0 {
        record.observed_timestamp_ns
    } else {
        record.timestamp_ns
    };
    let timestamp_ms = effective_timestamp_ns / 1_000_000;
    let timestamp = format_timestamp(timestamp_ms);

    // Determine source from service name
    let service_name = record.service_name.as_deref().unwrap_or("unknown");
    let source = if service_name.ends_with("_app") {
        "app"
    } else if service_name.ends_with("_ui") {
        " ui"
    } else if service_name.ends_with("_db") {
        " db"
    } else {
        "apx"
    };

    // Severity to channel
    let severity = record.severity_text.as_deref().unwrap_or("INFO");
    let channel = match severity.to_uppercase().as_str() {
        "ERROR" | "FATAL" | "CRITICAL" => "err",
        _ => "out",
    };

    let message = record.body.as_deref().unwrap_or("");

    // Colorize output
    let color_code = match source {
        "app" => "\x1b[36m", // cyan
        " ui" => "\x1b[35m", // magenta
        " db" => "\x1b[32m", // green
        _ => "\x1b[33m",     // yellow
    };
    let reset = "\x1b[0m";

    format!("{color_code}{timestamp} | {source} | {channel} | {message}{reset}")
}

/// Format a timestamp in milliseconds to `HH:MM:SS.mmm` format in local timezone.
fn format_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => {
            let local_dt = dt.with_timezone(&Local);
            local_dt.format("%H:%M:%S%.3f").to_string()
        }
        None => "??:??:??.???".to_string(),
    }
}

/// Check if a log record should be skipped (internal/noisy logs).
fn should_skip_log(record: &LogRecord) -> bool {
    let message = record.body.as_deref().unwrap_or("");
    let service_name = record.service_name.as_deref().unwrap_or("");
    let severity_number = record.severity_number.unwrap_or(9);

    // For apx service, only show INFO and higher (severity_number >= 5 is DEBUG)
    if service_name == "_core" && severity_number < 5 {
        return true;
    }

    // OpenTelemetry SDK internal logs
    if message.starts_with("BatchLogProcessor.")
        || message.starts_with("ReqwestBlockingClient.")
        || message.starts_with("HttpLogsClient.")
        || message.starts_with("HttpClient.")
        || message.starts_with("Http::connect")
    {
        return true;
    }

    // HTTP connection pooling logs
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

    // Other internal noise
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
