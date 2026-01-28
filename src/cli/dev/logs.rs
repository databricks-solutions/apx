//! Log viewer for APX dev server using flux SQLite storage.
//!
//! Reads logs from ~/.apx/logs/db which is maintained by flux.

use chrono::{Local, TimeZone, Utc};
use clap::Args;
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;

use crate::cli::run_cli_async;
use crate::dev::common::{lock_path, read_lock};
use crate::flux::{db_path, LogRecord, Storage};

pub const DEFAULT_LOG_DURATION: &str = "10m";

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

/// Minimum severity level for apx internal logs (DEBUG = 5, skipping TRACE = 1-4)
const APX_MIN_SEVERITY: i32 = 5;

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

    // Check if database exists
    let db_path = db_path()?;
    if !db_path.exists() {
        println!("⚠️  No logs database found at {}\n", db_path.display());
        println!("Logs will appear here once the dev server is started and produces output.");
        return Ok(());
    }

    // Open storage
    let storage = Storage::open().map_err(|e| format!("Failed to open logs database: {}", e))?;

    let duration = parse_duration(&args.duration)?;
    let since_ns = since_timestamp_nanos(duration);

    if args.follow {
        println!("📜 Streaming logs... (Ctrl+C to stop)\n");
        follow_logs(&storage, &app_path_canonical, since_ns, &lock_path).await
    } else {
        read_logs(&storage, &app_path_canonical, since_ns)
    }
}

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

    let storage = Storage::open().map_err(|e| format!("Failed to open logs database: {}", e))?;

    let duration = parse_duration(duration)?;
    let since_ns = since_timestamp_nanos(duration);

    let records = storage.query_logs(Some(&app_path_canonical), since_ns, None)?;
    let output: Vec<String> = records
        .iter()
        .filter(|r| !should_skip_log(r))
        .map(|r| format_log_record(r, false))
        .collect();
    Ok(output.join("\n"))
}

/// Read logs from database, filtered by app path and timestamp
fn read_logs(storage: &Storage, app_path: &str, since_ns: i64) -> Result<(), String> {
    let records = storage.query_logs(Some(app_path), since_ns, None)?;

    let filtered: Vec<_> = records.iter().filter(|r| !should_skip_log(r)).collect();

    if filtered.is_empty() {
        println!("No logs found for the specified time range.");
        return Ok(());
    }

    for record in filtered {
        println!("{}", format_log_record(record, true));
    }

    Ok(())
}

/// Follow logs for new entries
async fn follow_logs(
    storage: &Storage,
    app_path: &str,
    since_ns: i64,
    lock_path: &std::path::Path,
) -> Result<(), String> {
    // First, read existing logs
    read_logs(storage, app_path, since_ns)?;

    // Track last seen ID for incremental queries
    let mut last_id = storage.get_latest_id()?;

    // Track if server was initially running
    let server_was_running = lock_path.exists();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                debug!("Received Ctrl+C, stopping logs stream.");
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                // Poll for new logs
                let new_records = storage.query_logs_after_id(Some(app_path), last_id)?;

                for record in &new_records {
                    if !should_skip_log(record) {
                        println!("{}", format_log_record(record, true));
                    }
                }

                // Update last_id
                if let Ok(new_id) = storage.get_latest_id() {
                    if new_id > last_id {
                        last_id = new_id;
                    }
                }

                // Check if server was running but lockfile is now gone
                if server_was_running && !lock_path.exists() {
                    debug!("Dev server stopped (lockfile removed), exiting logs follow.");
                    println!("\n📭 Dev server stopped.");
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Format a log record for terminal display
fn format_log_record(record: &LogRecord, colorize: bool) -> String {
    // Per OTEL spec: use observed_timestamp_ns when timestamp_ns is 0/absent
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

    if colorize {
        let color_code = match source {
            "app" => "\x1b[36m", // cyan
            " ui" => "\x1b[35m", // magenta
            " db" => "\x1b[32m", // green
            _ => "\x1b[33m",    // yellow
        };
        let reset = "\x1b[0m";
        format!(
            "{color_code}{timestamp} | {source} | {channel} | {message}{reset}"
        )
    } else {
        format!("{timestamp} | {source} | {channel} | {message}")
    }
}

/// Check if a log record should be skipped (internal/noisy logs).
fn should_skip_log(record: &LogRecord) -> bool {
    let message = record.body.as_deref().unwrap_or("");
    let service_name = record.service_name.as_deref().unwrap_or("");
    let severity_number = record.severity_number.unwrap_or(9);

    // For apx service, only show INFO and higher
    if service_name == "_core" && severity_number < APX_MIN_SEVERITY {
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

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` format in local timezone.
fn format_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => {
            // Convert to local timezone for display
            let local_dt = dt.with_timezone(&Local);
            local_dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
        }
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

fn since_timestamp_nanos(duration: Duration) -> i64 {
    let now_ms = Utc::now().timestamp_millis() as u64;
    let now_ns = now_ms * 1_000_000;
    let duration_ns = duration.as_nanos() as u64;
    now_ns.saturating_sub(duration_ns) as i64
}
