//! Inline scrolling log display for dev server startup.
//!
//! Shows a fixed-height (5 lines) scrolling region that displays real-time logs
//! during server startup, then clears when complete.

use chrono::{Local, TimeZone, Utc};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::VecDeque;
use std::path::Path;

use crate::flux::{LogRecord, Storage};

/// Number of log lines to display in the scrolling window
const LOG_WINDOW_SIZE: usize = 5;

/// Inline scrolling log display using indicatif MultiProgress.
pub struct StartupLogDisplay {
    #[allow(dead_code)]
    multi: MultiProgress,
    lines: Vec<ProgressBar>,
    buffer: VecDeque<String>,
    last_log_id: i64,
    storage: Option<Storage>,
    app_path: String,
}

impl StartupLogDisplay {
    /// Create a new startup log display for the given app directory.
    pub fn new(app_dir: &Path) -> Self {
        let multi = MultiProgress::new();
        let style = ProgressStyle::with_template("{msg}").unwrap_or_else(|_| ProgressStyle::default_bar());

        let lines: Vec<_> = (0..LOG_WINDOW_SIZE)
            .map(|_| {
                let pb = multi.add(ProgressBar::new_spinner());
                pb.set_style(style.clone());
                pb.set_message("");
                pb
            })
            .collect();

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
            multi,
            lines,
            buffer: VecDeque::with_capacity(LOG_WINDOW_SIZE),
            last_log_id,
            storage,
            app_path,
        }
    }

    /// Poll for new logs and update the display.
    /// Returns the number of new log lines added.
    pub fn poll(&mut self) -> usize {
        let storage = match &self.storage {
            Some(s) => s,
            None => return 0,
        };

        // Query logs and collect lines to add (to avoid borrow conflicts)
        let records = match storage.query_logs_after_id(Some(&self.app_path), self.last_log_id) {
            Ok(r) => r,
            Err(_) => return 0,
        };

        let lines_to_add: Vec<_> = records
            .iter()
            .filter(|r| !should_skip_log(r))
            .map(|r| format_log_record(r))
            .collect();

        let new_last_id = storage.get_latest_id().ok();

        // Now we can mutably borrow self
        let added = lines_to_add.len();
        for line in lines_to_add {
            self.push_line(line);
        }

        // Update last_log_id
        if let Some(new_id) = new_last_id {
            if new_id > self.last_log_id {
                self.last_log_id = new_id;
            }
        }

        added
    }

    /// Add a log line, scrolling the display if necessary.
    fn push_line(&mut self, line: String) {
        self.buffer.push_back(line);
        if self.buffer.len() > LOG_WINDOW_SIZE {
            self.buffer.pop_front();
        }
        self.refresh();
    }

    /// Refresh all progress bars with current buffer contents.
    fn refresh(&self) {
        for (i, pb) in self.lines.iter().enumerate() {
            let msg = self.buffer.get(i).map(|s| s.as_str()).unwrap_or("");
            pb.set_message(msg.to_string());
        }
    }

    /// Clear the display (called on success or failure).
    pub fn finish_and_clear(&self) {
        for pb in &self.lines {
            pb.finish_and_clear();
        }
    }

}

/// Format a log record for terminal display (simplified version).
fn format_log_record(record: &LogRecord) -> String {
    let effective_timestamp_ns = if record.timestamp_ns == 0 {
        record.observed_timestamp_ns
    } else {
        record.timestamp_ns
    };
    let timestamp_ms = (effective_timestamp_ns / 1_000_000) as i64;
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
