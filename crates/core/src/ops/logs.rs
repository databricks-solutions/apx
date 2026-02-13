use chrono::{Local, TimeZone, Utc};
use std::collections::HashMap;
use std::time::Duration;

use apx_common::{LogRecord, Storage, db_path};

/// Time window for aggregating similar messages (in milliseconds)
const AGGREGATION_WINDOW_MS: i64 = 2000;

pub const DEFAULT_LOG_DURATION: &str = "10m";

/// Minimum severity level for apx internal logs (DEBUG = 5, skipping TRACE = 1-4)
const APX_MIN_SEVERITY: i32 = 5;

/// Fetch dev server logs for the given duration without following.
pub async fn fetch_logs(app_dir: &std::path::Path, duration: &str) -> Result<String, String> {
    let app_path_canonical = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf())
        .display()
        .to_string();

    let db_path = db_path()?;
    if !db_path.exists() {
        return Ok("No logs database found.".to_string());
    }

    let storage = Storage::open().map_err(|e| format!("Failed to open logs database: {e}"))?;

    let duration = parse_duration(duration)?;
    let since_ns = since_timestamp_nanos(duration);

    let records = storage.query_logs(Some(&app_path_canonical), since_ns, None)?;
    let filtered: Vec<_> = records.iter().filter(|r| !should_skip_log(r)).collect();

    let mut aggregator = LogAggregator::new();
    let mut output = Vec::new();

    for record in &filtered {
        let timestamp_ns = if record.timestamp_ns == 0 {
            record.observed_timestamp_ns
        } else {
            record.timestamp_ns
        };
        let timestamp_ms = timestamp_ns / 1_000_000;

        output.extend(aggregator.flush_expired(timestamp_ms, false));

        if !aggregator.add(record) {
            output.push(format_log_record(record, false));
        }
    }

    output.extend(aggregator.flush_all(false));

    Ok(output.join("\n"))
}

/// Format a log record for terminal display
pub fn format_log_record(record: &LogRecord, colorize: bool) -> String {
    let effective_timestamp_ns = if record.timestamp_ns == 0 {
        record.observed_timestamp_ns
    } else {
        record.timestamp_ns
    };
    let timestamp_ms = effective_timestamp_ns / 1_000_000;
    let timestamp = format_timestamp(timestamp_ms);

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

    let message = record.body.as_deref().unwrap_or("");

    if colorize {
        let color_code = match source {
            "app" => "\x1b[36m",
            " ui" => "\x1b[35m",
            " db" => "\x1b[32m",
            _ => "\x1b[33m",
        };
        let reset = "\x1b[0m";
        format!("{color_code}{timestamp} | {source} | {message}{reset}")
    } else {
        format!("{timestamp} | {source} | {message}")
    }
}

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

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` format in local timezone.
pub fn format_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => {
            let local_dt = dt.with_timezone(&Local);
            local_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
        }
        None => "????-??-?? ??:??:??.???".to_string(),
    }
}

pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Duration cannot be empty.".to_string());
    }
    let (value_str, unit) = match trimmed.chars().last() {
        Some(ch) if ch.is_ascii_digit() => (trimmed, 's'),
        Some(ch) => (&trimmed[..trimmed.len() - ch.len_utf8()], ch),
        None => return Err("Duration cannot be empty.".to_string()),
    };
    let value: u64 = value_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid duration value: {input}"))?;
    let seconds = match unit {
        's' | 'S' => value,
        'm' | 'M' => value
            .checked_mul(60)
            .ok_or_else(|| "Duration is too large.".to_string())?,
        'h' | 'H' => value
            .checked_mul(60 * 60)
            .ok_or_else(|| "Duration is too large.".to_string())?,
        'd' | 'D' => value
            .checked_mul(60 * 60 * 24)
            .ok_or_else(|| "Duration is too large.".to_string())?,
        _ => {
            return Err(
                "Invalid duration unit. Use s, m, h, or d (e.g. 30s, 10m, 1h).".to_string(),
            );
        }
    };
    Ok(Duration::from_secs(seconds))
}

pub fn since_timestamp_nanos(duration: Duration) -> i64 {
    let now_ms = Utc::now().timestamp_millis() as u64;
    let now_ns = now_ms * 1_000_000;
    let duration_ns = duration.as_nanos() as u64;
    now_ns.saturating_sub(duration_ns) as i64
}

/// Get aggregation key for a message if it should be aggregated.
fn get_aggregation_key(record: &LogRecord) -> Option<(String, &'static str)> {
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

/// Tracks aggregated messages within time windows
#[derive(Debug, Default)]
pub struct LogAggregator {
    buckets: HashMap<String, (usize, i64, i64, &'static str)>,
}

impl LogAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, record: &LogRecord) -> bool {
        let Some((key, template)) = get_aggregation_key(record) else {
            return false;
        };

        let timestamp_ns = if record.timestamp_ns == 0 {
            record.observed_timestamp_ns
        } else {
            record.timestamp_ns
        };
        let timestamp_ms = timestamp_ns / 1_000_000;

        let entry = self
            .buckets
            .entry(key)
            .or_insert((0, timestamp_ms, timestamp_ms, template));
        entry.0 += 1;
        entry.2 = timestamp_ms;

        true
    }

    pub fn flush_expired(&mut self, current_time_ms: i64, colorize: bool) -> Vec<String> {
        let mut output = Vec::new();
        let mut to_remove = Vec::new();

        for (key, (count, first_ts, last_ts, template)) in &self.buckets {
            if current_time_ms - last_ts > AGGREGATION_WINDOW_MS {
                if *count > 1 {
                    let formatted = format_aggregated(*count, *first_ts, template, colorize);
                    output.push(formatted);
                }
                to_remove.push(key.clone());
            }
        }

        for key in to_remove {
            self.buckets.remove(&key);
        }

        output
    }

    pub fn flush_all(&mut self, colorize: bool) -> Vec<String> {
        let mut output = Vec::new();

        for (count, first_ts, _last_ts, template) in self.buckets.values() {
            if *count > 1 {
                let formatted = format_aggregated(*count, *first_ts, template, colorize);
                output.push(formatted);
            }
        }

        self.buckets.clear();
        output
    }
}

fn format_aggregated(count: usize, timestamp_ms: i64, template: &str, colorize: bool) -> String {
    let timestamp = format_timestamp(timestamp_ms);
    let source = if template.starts_with("db") {
        " db"
    } else {
        "app"
    };
    let message = format!("[{count}] {template}");

    if colorize {
        let color_code = "\x1b[32m";
        let reset = "\x1b[0m";
        format!("{color_code}{timestamp} | {source} | {message}{reset}")
    } else {
        format!("{timestamp} | {source} | {message}")
    }
}
