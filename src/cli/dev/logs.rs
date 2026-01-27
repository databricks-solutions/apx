//! Log viewer for APX dev server using otelcol file output.
//!
//! Reads logs from ~/.apx/logs/logs.json which is written by otelcol.

use chrono::{TimeZone, Utc};
use clap::Args;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

use crate::cli::run_cli_async;
use crate::dev::common::{lock_path, read_lock};

pub const DEFAULT_LOG_DURATION: &str = "10m";

/// Directory where otelcol writes logs
const OTELCOL_LOGS_DIR: &str = ".apx/logs";

/// Log file name
const LOGS_FILENAME: &str = "logs.json";

#[derive(Args, Debug, Clone)]
pub struct LogsArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        short = 'd',
        long = "duration",
        default_value = DEFAULT_LOG_DURATION,
        value_name = "DURATION",
        help = "Duration to look back (e.g. 30s, 10m, 1h)"
    )]
    pub duration: String,
    #[arg(short = 'f', long = "follow", help = "Follow logs until Ctrl+C")]
    pub follow: bool,
}

/// A parsed log entry from OTLP JSON format
#[derive(Debug, Clone)]
struct LogEntry {
    timestamp_ns: u64,
    severity: String,
    message: String,
    service_name: String,
    app_path: Option<String>,
}

/// Minimum severity level for apx internal logs (DEBUG = 5, skipping TRACE = 1-4)
const APX_MIN_SEVERITY: u8 = 5;

impl LogEntry {
    /// Format for terminal display
    fn format(&self, colorize: bool) -> String {
        let timestamp_ms = (self.timestamp_ns / 1_000_000) as i64;
        let timestamp = format_timestamp(timestamp_ms);

        // Determine source from service name
        let source = if self.service_name.ends_with("_backend") {
            "app"
        } else if self.service_name == "browser" {
            " ui"
        } else if self.service_name.ends_with("_db") {
            " db"
        } else {
            "apx"
        };

        // Severity to channel
        let channel = match self.severity.to_uppercase().as_str() {
            "ERROR" | "FATAL" | "CRITICAL" => "err",
            _ => "out",
        };

        if colorize {
            let color_code = match source {
                "app" => "\x1b[36m", // cyan
                " ui" => "\x1b[35m", // magenta
                " db" => "\x1b[32m", // green
                _ => "\x1b[33m",     // yellow
            };
            let reset = "\x1b[0m";
            format!(
                "{color_code}{timestamp} | {source} | {channel} | {}{reset}",
                self.message
            )
        } else {
            format!("{timestamp} | {source} | {channel} | {}", self.message)
        }
    }
}

pub async fn run(args: LogsArgs) -> i32 {
    run_cli_async(|| run_async(args)).await
}

async fn run_async(args: LogsArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Canonicalize path for matching
    let app_path_canonical = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.clone())
        .display()
        .to_string();

    // Check if dev server is running (optional - logs may exist even if server stopped)
    let lock_path = lock_path(&app_dir);
    if !lock_path.exists() {
        debug!("No dev server lockfile found, but will still try to read logs.");
    } else {
        let lock = read_lock(&lock_path)?;
        debug!(port = lock.port, "Dev server running at port.");
    }

    // Find logs file
    let logs_path = get_logs_path()?;
    if !logs_path.exists() {
        println!("⚠️  No logs found at {}\n", logs_path.display());
        println!("Logs will appear here once the dev server is started and produces output.");
        return Ok(());
    }

    let duration = parse_duration(&args.duration)?;
    let since_ns = since_timestamp_nanos(duration);

    if args.follow {
        println!("📜 Streaming logs... (Ctrl+C to stop)\n");
        follow_logs(&logs_path, &app_path_canonical, since_ns).await
    } else {
        read_logs(&logs_path, &app_path_canonical, since_ns)
    }
}

/// Fetch dev server logs for the given duration without following.
pub async fn fetch_logs(app_dir: &Path, duration: &str) -> Result<String, String> {
    let app_path_canonical = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf())
        .display()
        .to_string();

    let logs_path = get_logs_path()?;
    if !logs_path.exists() {
        return Ok("No logs file found.".to_string());
    }

    let duration = parse_duration(duration)?;
    let since_ns = since_timestamp_nanos(duration);

    let entries = read_log_entries(&logs_path, &app_path_canonical, since_ns)?;
    let output: Vec<String> = entries.iter().map(|e| e.format(false)).collect();
    Ok(output.join("\n"))
}

/// Get the path to the logs file
fn get_logs_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(OTELCOL_LOGS_DIR).join(LOGS_FILENAME))
}

/// Read logs from file, filtered by app path and timestamp
fn read_logs(logs_path: &Path, app_path: &str, since_ns: u64) -> Result<(), String> {
    let entries = read_log_entries(logs_path, app_path, since_ns)?;

    if entries.is_empty() {
        println!("No logs found for the specified time range.");
        return Ok(());
    }

    for entry in entries {
        println!("{}", entry.format(true));
    }

    Ok(())
}

/// Read and parse log entries from file
fn read_log_entries(
    logs_path: &Path,
    app_path: &str,
    since_ns: u64,
) -> Result<Vec<LogEntry>, String> {
    let file =
        File::open(logs_path).map_err(|e| format!("Failed to open logs file: {}", e))?;
    let reader = BufReader::new(file);

    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Failed to read log line: {}", e))?;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(parsed) = parse_otlp_line(&line) {
            for entry in parsed {
                // Filter by app path if specified
                if let Some(ref entry_app_path) = entry.app_path {
                    if !entry_app_path.contains(app_path) && !app_path.contains(entry_app_path) {
                        continue;
                    }
                }

                // Filter by timestamp
                if entry.timestamp_ns >= since_ns {
                    entries.push(entry);
                }
            }
        }
    }

    // Sort by timestamp
    entries.sort_by_key(|e| e.timestamp_ns);
    Ok(entries)
}

/// Follow logs file for new entries
async fn follow_logs(logs_path: &Path, app_path: &str, since_ns: u64) -> Result<(), String> {
    // First, read existing logs
    read_logs(logs_path, app_path, since_ns)?;

    // Set up file watcher
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(100);

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    // Watch the logs directory
    let logs_dir = logs_path
        .parent()
        .ok_or("Invalid logs path")?;
    watcher
        .watch(logs_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch logs directory: {}", e))?;

    // Track file position for incremental reading
    let mut file = File::open(logs_path).map_err(|e| format!("Failed to open logs file: {}", e))?;
    let mut file_pos = file
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("Failed to seek logs file: {}", e))?;

    // Buffer for recently seen entries to avoid duplicates
    let mut recent_entries: VecDeque<u64> = VecDeque::with_capacity(100);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                debug!("Received Ctrl+C, stopping logs stream.");
                break;
            }
            event = rx.recv() => {
                if event.is_none() {
                    break;
                }

                // Read new content from file
                if let Ok(metadata) = file.metadata() {
                    let new_len = metadata.len();
                    if new_len > file_pos {
                        // Seek to last position and read new content
                        if file.seek(SeekFrom::Start(file_pos)).is_ok() {
                            let reader = BufReader::new(&file);
                            for line in reader.lines().map_while(Result::ok) {
                                if line.trim().is_empty() {
                                    continue;
                                }

                                if let Ok(parsed) = parse_otlp_line(&line) {
                                    for entry in parsed {
                                        // Filter by app path
                                        if let Some(ref entry_app_path) = entry.app_path {
                                            if !entry_app_path.contains(app_path) && !app_path.contains(entry_app_path) {
                                                continue;
                                            }
                                        }

                                        // Deduplicate
                                        if recent_entries.contains(&entry.timestamp_ns) {
                                            continue;
                                        }
                                        recent_entries.push_back(entry.timestamp_ns);
                                        if recent_entries.len() > 100 {
                                            recent_entries.pop_front();
                                        }

                                        println!("{}", entry.format(true));
                                    }
                                }
                            }
                        }
                        file_pos = new_len;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // Periodic check for file changes (in case notify misses some)
            }
        }
    }

    Ok(())
}

/// Parse a single line of OTLP JSON format
fn parse_otlp_line(line: &str) -> Result<Vec<LogEntry>, String> {
    let json: serde_json::Value =
        serde_json::from_str(line).map_err(|e| format!("Invalid JSON: {}", e))?;

    let mut entries = Vec::new();

    let empty_vec: Vec<serde_json::Value> = vec![];
    let resource_logs = json
        .get("resourceLogs")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_vec);

    for resource_log in resource_logs {
        // Extract resource attributes
        let mut service_name = String::from("unknown");
        let mut app_path = None;

        if let Some(resource) = resource_log.get("resource") {
            if let Some(attrs) = resource.get("attributes").and_then(|v| v.as_array()) {
                for attr in attrs {
                    let key = attr.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    let value = attr
                        .get("value")
                        .and_then(|v| v.get("stringValue"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    match key {
                        "service.name" => service_name = value.to_string(),
                        "apx.app_path" => app_path = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }

        // Extract log records
        let empty_scope_logs: Vec<serde_json::Value> = vec![];
        let scope_logs = resource_log
            .get("scopeLogs")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_scope_logs);

        for scope_log in scope_logs {
            let empty_log_records: Vec<serde_json::Value> = vec![];
            let log_records = scope_log
                .get("logRecords")
                .and_then(|v| v.as_array())
                .unwrap_or(&empty_log_records);

            for record in log_records {
                // Try timeUnixNano first, fall back to observedTimeUnixNano
                let timestamp_ns = record
                    .get("timeUnixNano")
                    .or_else(|| record.get("observedTimeUnixNano"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                let severity = record
                    .get("severityText")
                    .and_then(|v| v.as_str())
                    .unwrap_or("INFO")
                    .to_string();

                let severity_number = record
                    .get("severityNumber")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(9) as u8; // Default to INFO

                // Try body.stringValue first, fall back to eventName
                let body_str = record
                    .get("body")
                    .and_then(|v| v.get("stringValue"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                
                let message = if body_str.is_empty() {
                    record
                        .get("eventName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    body_str.to_string()
                };

                // Skip internal/noisy logs
                if should_skip_log(&message) {
                    continue;
                }

                // For apx service, only show INFO and higher
                if service_name == "_core" && severity_number < APX_MIN_SEVERITY {
                    continue;
                }

                entries.push(LogEntry {
                    timestamp_ns,
                    severity,
                    message,
                    service_name: service_name.clone(),
                    app_path: app_path.clone(),
                });
            }
        }
    }

    Ok(entries)
}

/// Check if a log message should be skipped (internal/noisy logs)
fn should_skip_log(message: &str) -> bool {
    // OpenTelemetry SDK internal logs
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

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` format.
fn format_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        None => "????-??-?? ??:??:??.???".to_string(),
    }
}

fn parse_duration(input: &str) -> Result<Duration, String> {
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
            )
        }
    };
    Ok(Duration::from_secs(seconds))
}

fn since_timestamp_nanos(duration: Duration) -> u64 {
    let now_ms = Utc::now().timestamp_millis() as u64;
    let now_ns = now_ms * 1_000_000;
    let duration_ns = duration.as_nanos() as u64;
    now_ns.saturating_sub(duration_ns)
}
