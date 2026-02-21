//! Centralized log formatting, timestamp formatting, and severity utilities.
//!
//! All user-facing timestamps use the **local** timezone and a consistent pattern.
//! This module is the single source of truth for log presentation across all APX crates.

use chrono::{Local, TimeZone, Utc};

use crate::{AggregatedRecord, LogRecord, source_label};

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` in local timezone.
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

/// Format a timestamp in milliseconds to `HH:MM:SS.mmm` in local timezone.
pub fn format_short_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => {
            let local_dt = dt.with_timezone(&Local);
            local_dt.format("%H:%M:%S%.3f").to_string()
        }
        None => "??:??:??.???".to_string(),
    }
}

/// Format a log record for terminal display.
///
/// Output: `2026-01-28 16:09:02.413 | app | <message>`
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

/// Format a log record for startup display (compact timestamp, always colorized, with channel).
pub fn format_startup_log(record: &LogRecord) -> String {
    let timestamp = format_timestamp(record.effective_timestamp_ms());

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

    let severity = record.severity_text.as_deref().unwrap_or("INFO");
    let channel = match severity.to_uppercase().as_str() {
        "ERROR" | "FATAL" | "CRITICAL" => "err",
        _ => "out",
    };

    let message = record.body.as_deref().unwrap_or("");

    let color_code = match source {
        "app" => "\x1b[36m", // cyan
        " ui" => "\x1b[35m", // magenta
        " db" => "\x1b[32m", // green
        _ => "\x1b[33m",     // yellow
    };
    let reset = "\x1b[0m";

    format!("{color_code}{timestamp} | {source} | {channel} | {message}{reset}")
}

/// Format a subprocess log line with local timestamp and source prefix.
///
/// Output: `2026-01-28 16:09:02.413 |  app | <message>`
pub fn format_process_log_line(source: &str, message: &str) -> String {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.3f");
    format!("{timestamp} | {source:>4} | {message}")
}

/// ANSI color code for a source label.
pub fn source_color(src: &str) -> &'static str {
    match src {
        "app" => "\x1b[36m",
        "ui" => "\x1b[35m",
        "db" => "\x1b[32m",
        _ => "\x1b[33m",
    }
}

/// Convert severity level string to OTLP severity number.
pub fn severity_to_number(level: &str) -> u8 {
    match level.to_uppercase().as_str() {
        "TRACE" => 1,
        "DEBUG" => 5,
        "INFO" | "LOG" => 9,
        "WARN" | "WARNING" => 13,
        "ERROR" => 17,
        "FATAL" | "CRITICAL" => 21,
        _ => 9, // default to INFO
    }
}

/// Parse severity from a Python/uvicorn log line.
///
/// Matches patterns like:
/// - `"INFO     ..."` or `"INFO    / ..."` (uvicorn formatted)
/// - `"INFO:    ..."` (basic Python logging)
/// - `"WARNING  ..."`, `"ERROR    ..."`, `"DEBUG    ..."` etc.
///
/// Returns `"INFO"` if no level found (most stderr is informational).
pub fn parse_python_severity(line: &str) -> &'static str {
    let trimmed = line.trim_start();

    // Check for Python/uvicorn patterns: "LEVEL" followed by whitespace, ":", or "/"
    for (prefix, severity) in [
        ("INFO", "INFO"),
        ("WARNING", "WARNING"),
        ("WARN", "WARNING"),
        ("ERROR", "ERROR"),
        ("DEBUG", "DEBUG"),
        ("CRITICAL", "CRITICAL"),
        ("FATAL", "FATAL"),
    ] {
        if trimmed.len() > prefix.len() {
            let after = trimmed.as_bytes().get(prefix.len());
            if trimmed.starts_with(prefix) && matches!(after, Some(b' ' | b':' | b'/' | b'\t')) {
                return severity;
            }
        }
    }

    // Default: treat as INFO (most uvicorn stderr output is informational)
    "INFO"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_to_number() {
        assert_eq!(severity_to_number("TRACE"), 1);
        assert_eq!(severity_to_number("DEBUG"), 5);
        assert_eq!(severity_to_number("INFO"), 9);
        assert_eq!(severity_to_number("WARN"), 13);
        assert_eq!(severity_to_number("WARNING"), 13);
        assert_eq!(severity_to_number("ERROR"), 17);
        assert_eq!(severity_to_number("FATAL"), 21);
        assert_eq!(severity_to_number("unknown"), 9);
    }

    #[test]
    fn test_parse_python_severity_uvicorn() {
        assert_eq!(
            parse_python_severity("INFO     Started server process [1234]"),
            "INFO"
        );
        assert_eq!(
            parse_python_severity("INFO:    Uvicorn running on http://0.0.0.0:8000"),
            "INFO"
        );
        assert_eq!(
            parse_python_severity("WARNING  Invalid configuration"),
            "WARNING"
        );
        assert_eq!(parse_python_severity("ERROR    Something failed"), "ERROR");
        assert_eq!(parse_python_severity("DEBUG    Detailed info"), "DEBUG");
    }

    #[test]
    fn test_parse_python_severity_default() {
        assert_eq!(parse_python_severity("Just a regular message"), "INFO");
        assert_eq!(
            parse_python_severity("Uvicorn running on http://0.0.0.0:8000"),
            "INFO"
        );
        assert_eq!(parse_python_severity(""), "INFO");
    }

    #[test]
    fn test_parse_python_severity_with_slash() {
        assert_eq!(
            parse_python_severity("INFO/ uvicorn.error something"),
            "INFO"
        );
        assert_eq!(
            parse_python_severity("WARNING/ some.module warning"),
            "WARNING"
        );
    }
}
