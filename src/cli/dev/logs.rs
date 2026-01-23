use chrono::{TimeZone, Utc};
use clap::Args;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_stream::StreamExt;
use tracing::{debug, warn};

use crate::cli::run_cli_async;
use crate::dev::client::logs;
use crate::dev::common::{lock_path, read_lock};
use crate::dev::logging::{decode_log_payload, LogPayload, LogPipe, LogStreamName, APX_SHUTDOWN_MESSAGE};

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
    #[arg(short = 'f', long = "follow", help = "Follow logs until the server stops")]
    pub follow: bool,
}

pub async fn run(args: LogsArgs) -> i32 {
    run_cli_async(|| run_async(args)).await
}

async fn run_async(args: LogsArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let lock_path = lock_path(&app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        println!("⚠️  No dev server running\n");
        return Ok(());
    }

    let lock = read_lock(&lock_path)?;
    debug!(port = lock.port, "Connecting to dev server logs.");
    let duration = parse_duration(&args.duration)?;
    let since = Some(since_timestamp_millis(duration));
    let response = logs(lock.port, since, args.follow).await?;
    if !response.status().is_success() {
        return Err(format!(
            "Logs request failed with status {}",
            response.status()
        ));
    }

    if args.follow {
        println!("📜 Streaming logs...\n");
    }

    stream_logs(response, args.follow).await
}

/// Fetch dev server logs for the given duration without following.
pub async fn fetch_logs(app_dir: &Path, duration: &str) -> Result<String, String> {
    let lock_path = lock_path(app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        return Ok("No dev server lockfile found.".to_string());
    }

    let lock = read_lock(&lock_path)?;
    debug!(port = lock.port, "Connecting to dev server logs.");
    let duration = parse_duration(duration)?;
    let since = Some(since_timestamp_millis(duration));
    let response = logs(lock.port, since, false).await?;
    if !response.status().is_success() {
        return Err(format!(
            "Logs request failed with status {}",
            response.status()
        ));
    }

    collect_logs(response).await
}

/// Collect logs from a non-following logs response.
pub async fn collect_logs(response: reqwest::Response) -> Result<String, String> {
    let stream = response.bytes_stream();
    let reader = BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|result| {
            result.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
        }),
    ));
    let mut lines = reader.lines();
    let mut data_lines: Vec<String> = Vec::new();
    let mut output: Vec<String> = Vec::new();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                process_line_collect(&line, &mut data_lines, &mut output)?;
            }
            Ok(None) => {
                break;
            }
            Err(err) => {
                flush_log_payload(&mut data_lines, &mut output);
                return Err(format!("Failed to read logs: {err}"));
            }
        }
    }

    flush_log_payload(&mut data_lines, &mut output);
    Ok(output.join("\n"))
}

pub async fn stream_logs(response: reqwest::Response, follow: bool) -> Result<(), String> {
    let stream = response.bytes_stream();
    let reader = BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|result| result.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))),
    ));
    let mut lines = reader.lines();
    let mut data_lines: Vec<String> = Vec::new();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                debug!("Received Ctrl+C, stopping logs stream.");
                break;
            }
            line_result = lines.next_line() => {
                match line_result {
                    Ok(Some(line)) => {
                        if let Some(should_exit) = process_line(&line, &mut data_lines, follow) {
                            if should_exit {
                                return Ok(());
                            }
                        }
                    }
                    Ok(None) => {
                        // Stream ended
                        break;
                    }
                    Err(err) => {
                        if !data_lines.is_empty() {
                            let data = data_lines.join("\n");
                            data_lines.clear();
                            handle_log_payload(&data);
                        }
                        if follow {
                            // Treat connection errors during follow as graceful shutdowns.
                            debug!(error = %err, "Logs stream ended during follow.");
                            return Ok(());
                        }
                        return Err(format!("Failed to read logs: {err}"));
                    }
                }
            }
        }
    }

    // Process any remaining data
    if !data_lines.is_empty() {
        let data = data_lines.join("\n");
        handle_log_payload(&data);
    }

    Ok(())
}

/// Process a line from the SSE stream.
/// Returns Some(true) if we should exit, Some(false) to continue, None if line was just processed.
pub fn process_line(line: &str, data_lines: &mut Vec<String>, follow: bool) -> Option<bool> {
    let line = line.trim_end_matches(['\n', '\r']);

    if line.is_empty() {
        if !data_lines.is_empty() {
            let data = data_lines.join("\n");
            data_lines.clear();
            if handle_log_payload(&data) && follow {
                return Some(true);
            }
        }
        return None;
    }

    if line.starts_with(':') {
        return None;
    }

    if let Some(payload) = line.strip_prefix("data:") {
        let payload = payload.trim_start();
        if payload.is_empty() || payload == "keep-alive" {
            return None;
        }
        data_lines.push(payload.to_string());
    }

    Some(false)
}

pub fn handle_log_payload(data: &str) -> bool {
    match format_log_payload(data, true) {  // colorize for terminal output
        Ok((line, should_exit)) => {
            println!("{line}");
            should_exit
        }
        Err(err) => {
            warn!(error = %err, raw = data, "Failed to parse log payload.");
            false
        }
    }
}

/// Format a timestamp in milliseconds to `YYYY-MM-DD HH:MM:SS.mmm` format.
fn format_timestamp(timestamp_ms: i64) -> String {
    let datetime = Utc.timestamp_millis_opt(timestamp_ms).single();
    match datetime {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        None => "????-??-?? ??:??:??.???".to_string(),
    }
}

/// Format a log payload into a formatted line.
/// Returns (formatted_line, should_exit).
pub fn format_log_payload_struct(payload: &LogPayload, colorize: bool) -> (String, bool) {
    // Fixed-width source names (3 chars)
    let source = match payload.stream {
        LogStreamName::App => "app",
        LogStreamName::Ui => " ui",
        LogStreamName::Apx => "apx",
        LogStreamName::Db => " db",
    };
    
    // Fixed-width channel names (3 chars)
    let channel = match payload.pipe {
        Some(LogPipe::Out) => "out",
        Some(LogPipe::Error) => "err",
        None => "---",
    };
    
    let timestamp = format_timestamp(payload.timestamp);
    
    let line = if colorize {
        // Color codes for terminal output
        let color_code = match payload.stream {
            LogStreamName::App => "\x1b[36m", // cyan
            LogStreamName::Ui => "\x1b[35m",  // magenta
            LogStreamName::Apx => "\x1b[33m", // yellow
            LogStreamName::Db => "\x1b[32m",  // green
        };
        let reset = "\x1b[0m";
        format!("{color_code}{timestamp} | {source} | {channel} | {}{reset}", payload.message)
    } else {
        // Plain text for MCP and other non-terminal outputs
        format!("{timestamp} | {source} | {channel} | {}", payload.message)
    };
    
    let should_exit = payload.stream == LogStreamName::Apx && payload.message == APX_SHUTDOWN_MESSAGE;
    (line, should_exit)
}

fn format_log_payload(data: &str, colorize: bool) -> Result<(String, bool), String> {
    let payload = decode_log_payload(data)?;
    Ok(format_log_payload_struct(&payload, colorize))
}

fn process_line_collect(
    line: &str,
    data_lines: &mut Vec<String>,
    output: &mut Vec<String>,
) -> Result<(), String> {
    let line = line.trim_end_matches(['\n', '\r']);

    if line.is_empty() {
        flush_log_payload(data_lines, output);
        return Ok(());
    }

    if line.starts_with(':') {
        return Ok(());
    }

    if let Some(payload) = line.strip_prefix("data:") {
        let payload = payload.trim_start();
        if payload.is_empty() || payload == "keep-alive" {
            return Ok(());
        }
        data_lines.push(payload.to_string());
    }

    Ok(())
}

fn flush_log_payload(data_lines: &mut Vec<String>, output: &mut Vec<String>) {
    if data_lines.is_empty() {
        return;
    }
    let data = data_lines.join("\n");
    data_lines.clear();
    match format_log_payload(&data, false) {  // no colors for MCP/non-terminal output
        Ok((line, _)) => output.push(line),
        Err(err) => {
            warn!(error = %err, raw = data, "Failed to parse log payload.");
        }
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

fn since_timestamp_millis(duration: Duration) -> i64 {
    let now = Utc::now().timestamp_millis();
    let millis: i64 = duration.as_millis().try_into().unwrap_or(i64::MAX);
    now.saturating_sub(millis)
}
