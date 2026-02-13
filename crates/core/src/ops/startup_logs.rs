//! Log streaming for dev server startup.
//!
//! Prints real-time logs line-by-line during server startup.

use chrono::{Local, TimeZone, Utc};
use std::path::Path;

use apx_common::{LogRecord, Storage};

use super::logs::should_skip_log;

/// Simple log streamer that prints logs line-by-line to stdout.
pub struct StartupLogStreamer {
    last_log_id: i64,
    storage: Option<Storage>,
    app_path: String,
}

impl std::fmt::Debug for StartupLogStreamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupLogStreamer")
            .field("last_log_id", &self.last_log_id)
            .field("storage", &self.storage.as_ref().map(|_| ".."))
            .field("app_path", &self.app_path)
            .finish()
    }
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
                println!("{}", format_startup_log(record));
                count += 1;
            }
        }

        // Update last_log_id
        if let Ok(new_id) = storage.get_latest_id()
            && new_id > self.last_log_id
        {
            self.last_log_id = new_id;
        }

        count
    }
}

/// Format a log record for startup display (compact timestamp, always colorized, with channel).
fn format_startup_log(record: &LogRecord) -> String {
    let effective_timestamp_ns = if record.timestamp_ns == 0 {
        record.observed_timestamp_ns
    } else {
        record.timestamp_ns
    };
    let timestamp_ms = effective_timestamp_ns / 1_000_000;
    let timestamp = format_short_timestamp(timestamp_ms);

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
fn format_short_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => {
            let local_dt = dt.with_timezone(&Local);
            local_dt.format("%H:%M:%S%.3f").to_string()
        }
        None => "??:??:??.???".to_string(),
    }
}
