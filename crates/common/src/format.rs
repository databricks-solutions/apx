//! Centralized log formatting, timestamp formatting, and severity utilities.
//!
//! All user-facing timestamps use the **local** timezone and a consistent pattern.
//! This module is the single source of truth for log presentation across all APX crates.

use chrono::{Local, TimeZone, Utc};

use crate::{AggregatedRecord, LogRecord, ServiceKind};

// ANSI color codes for terminal output.
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RESET: &str = "\x1b[0m";

impl ServiceKind {
    /// ANSI color escape for this service kind.
    #[must_use]
    pub const fn ansi_color(self) -> &'static str {
        match self {
            Self::App => ANSI_CYAN,
            Self::Ui => ANSI_MAGENTA,
            Self::Db => ANSI_GREEN,
            Self::Other => ANSI_YELLOW,
        }
    }
}

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` in local timezone.
#[must_use]
pub fn format_timestamp(timestamp_ms: i64) -> String {
    Utc.timestamp_millis_opt(timestamp_ms).single().map_or_else(
        || "????-??-?? ??:??:??.???".to_string(),
        |dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string()
        },
    )
}

/// Format a timestamp in milliseconds to `HH:MM:SS.mmm` in local timezone.
#[must_use]
pub fn format_short_timestamp(timestamp_ms: i64) -> String {
    Utc.timestamp_millis_opt(timestamp_ms).single().map_or_else(
        || "??:??:??.???".to_string(),
        |dt| dt.with_timezone(&Local).format("%H:%M:%S%.3f").to_string(),
    )
}

/// Format a log record for terminal display.
///
/// Output: `2026-01-28 16:09:02.413 | app | <message>`
#[must_use]
pub fn format_log_record(record: &LogRecord, colorize: bool) -> String {
    let kind = ServiceKind::from_service_name(record.service_name.as_deref().unwrap_or("unknown"));
    format_line(
        &format_timestamp(record.effective_timestamp_ms()),
        kind,
        record.body.as_deref().unwrap_or(""),
        colorize,
    )
}

/// Format an aggregated record for terminal display.
#[must_use]
pub fn format_aggregated_record(agg: &AggregatedRecord, colorize: bool) -> String {
    let kind = ServiceKind::from_service_name(&agg.service_name);
    let message = format!("[{}] {}", agg.count, agg.template);
    format_line(
        &format_timestamp(agg.timestamp_ms),
        kind,
        &message,
        colorize,
    )
}

/// Format a log record for startup display (compact timestamp, always colorized, with channel).
#[must_use]
pub fn format_startup_log(record: &LogRecord) -> String {
    let timestamp = format_timestamp(record.effective_timestamp_ms());
    let kind = ServiceKind::from_service_name(record.service_name.as_deref().unwrap_or("unknown"));

    let severity = record.severity_text.as_deref().unwrap_or("INFO");
    let channel = match severity.to_uppercase().as_str() {
        "ERROR" | "FATAL" | "CRITICAL" => "err",
        _ => "out",
    };

    let message = record.body.as_deref().unwrap_or("");
    let label = kind.label();
    let color = kind.ansi_color();

    format!("{color}{timestamp} | {label:>3} | {channel} | {message}{ANSI_RESET}")
}

/// Format a subprocess log line with local timestamp and source prefix.
///
/// Output: `2026-01-28 16:09:02.413 |  app | <message>`
#[must_use]
pub fn format_process_log_line(source: &str, message: &str) -> String {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.3f");
    format!("{timestamp} | {source:>4} | {message}")
}

/// ANSI color code for a source label.
#[must_use]
pub fn source_color(src: &str) -> &'static str {
    match src {
        "app" => ANSI_CYAN,
        "ui" => ANSI_MAGENTA,
        "db" => ANSI_GREEN,
        _ => ANSI_YELLOW,
    }
}

/// Shared formatter for `timestamp | src | message` lines.
fn format_line(timestamp: &str, kind: ServiceKind, message: &str, colorize: bool) -> String {
    let label = kind.label();
    if colorize {
        let color = kind.ansi_color();
        format!("{color}{timestamp} | {label:>3} | {message}{ANSI_RESET}")
    } else {
        format!("{timestamp} | {label:>3} | {message}")
    }
}

/// Convert severity level string to OTLP severity number.
#[must_use]
pub fn severity_to_number(level: &str) -> u8 {
    match level.to_uppercase().as_str() {
        "TRACE" => 1,
        "DEBUG" => 5,
        "WARN" | "WARNING" => 13,
        "ERROR" => 17,
        "FATAL" | "CRITICAL" => 21,
        _ => 9, // INFO, LOG, and unknown levels default to INFO
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
#[must_use]
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
