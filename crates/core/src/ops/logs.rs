use chrono::{Local, TimeZone, Utc};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

use apx_common::{AggregatedRecord, LogAggregator, LogRecord, should_skip_log, source_label};
use apx_db::LogsDb;

pub const DEFAULT_LOG_DURATION: &str = "10m";

// ---------------------------------------------------------------------------
// Shared query helper
// ---------------------------------------------------------------------------

/// Query and filter logs for the given app directory and duration string.
async fn query_filtered_logs(app_dir: &Path, duration: &str) -> Result<Vec<LogRecord>, String> {
    let app_path_canonical = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf())
        .display()
        .to_string();

    let db_path = apx_db::logs_db_path()?;
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let storage = LogsDb::open()
        .await
        .map_err(|e| format!("Failed to open logs database: {e}"))?;

    let duration = parse_duration(duration)?;
    let since_ns = since_timestamp_nanos(duration);

    let records = storage
        .query_logs(Some(&app_path_canonical), since_ns, None)
        .await?;
    Ok(records
        .into_iter()
        .filter(|r| !should_skip_log(r))
        .collect())
}

// ---------------------------------------------------------------------------
// Public fetch functions
// ---------------------------------------------------------------------------

/// Fetch dev server logs for the given duration without following.
pub async fn fetch_logs(app_dir: &Path, duration: &str) -> Result<String, String> {
    let filtered = query_filtered_logs(app_dir, duration).await?;

    if filtered.is_empty() {
        let db_path = apx_db::logs_db_path()?;
        if !db_path.exists() {
            return Ok("No logs database found.".to_string());
        }
    }

    let mut aggregator = LogAggregator::new();
    let mut output = Vec::new();

    for record in &filtered {
        let timestamp_ms = record.effective_timestamp_ms();

        for agg in aggregator.flush_expired(timestamp_ms) {
            output.push(format_aggregated_record(&agg, false));
        }

        if !aggregator.add(record) {
            output.push(format_log_record(record, false));
        }
    }

    for agg in aggregator.flush_all() {
        output.push(format_aggregated_record(&agg, false));
    }

    Ok(output.join("\n"))
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub source: String,
    pub severity: Option<String>,
    pub message: String,
}

/// Fetch dev server logs as structured entries (for MCP / programmatic use).
pub async fn fetch_logs_structured(
    app_dir: &Path,
    duration: &str,
) -> Result<Vec<LogEntry>, String> {
    let filtered = query_filtered_logs(app_dir, duration).await?;

    let mut aggregator = LogAggregator::new();
    let mut entries = Vec::new();

    for record in &filtered {
        let timestamp_ms = record.effective_timestamp_ms();

        for agg in aggregator.flush_expired(timestamp_ms) {
            entries.push(aggregated_record_to_entry(&agg));
        }

        if !aggregator.add(record) {
            entries.push(log_record_to_entry(record));
        }
    }

    for agg in aggregator.flush_all() {
        entries.push(aggregated_record_to_entry(&agg));
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Formatting (presentation layer)
// ---------------------------------------------------------------------------

fn log_record_to_entry(record: &LogRecord) -> LogEntry {
    LogEntry {
        timestamp: format_timestamp(record.effective_timestamp_ms()),
        source: record.source_label().to_string(),
        severity: record.severity_text.clone(),
        message: record.body.as_deref().unwrap_or("").to_string(),
    }
}

fn aggregated_record_to_entry(agg: &AggregatedRecord) -> LogEntry {
    LogEntry {
        timestamp: format_timestamp(agg.timestamp_ms),
        source: source_label(&agg.service_name).to_string(),
        severity: None,
        message: format!("[{}] {}", agg.count, agg.template),
    }
}

/// Format a log record for terminal display.
pub fn format_log_record(record: &LogRecord, colorize: bool) -> String {
    let timestamp = format_timestamp(record.effective_timestamp_ms());
    let src = record.source_label();
    let padded_src = format!("{src:>3}");
    let message = record.body.as_deref().unwrap_or("");

    if colorize {
        let color_code = source_color(src);
        let reset = "\x1b[0m";
        format!("{color_code}{timestamp} | {padded_src} | {message}{reset}")
    } else {
        format!("{timestamp} | {padded_src} | {message}")
    }
}

/// Format an aggregated record for terminal display.
pub fn format_aggregated_record(agg: &AggregatedRecord, colorize: bool) -> String {
    let timestamp = format_timestamp(agg.timestamp_ms);
    let src = source_label(&agg.service_name);
    let padded_src = format!("{src:>3}");
    let message = format!("[{}] {}", agg.count, agg.template);

    if colorize {
        let color_code = source_color(src);
        let reset = "\x1b[0m";
        format!("{color_code}{timestamp} | {padded_src} | {message}{reset}")
    } else {
        format!("{timestamp} | {padded_src} | {message}")
    }
}

fn source_color(src: &str) -> &'static str {
    match src {
        "app" => "\x1b[36m",
        "ui" => "\x1b[35m",
        "db" => "\x1b[32m",
        _ => "\x1b[33m",
    }
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
