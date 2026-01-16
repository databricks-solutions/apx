use clap::Args;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use tracing::{debug, warn};

use crate::cli::run_cli;
use crate::dev::client::logs as logs_request;
use crate::dev::common::{lock_path, read_lock};
use crate::dev::logging::{decode_log_payload, LogPipe, LogStreamName};

#[derive(Args, Debug, Clone)]
pub struct LogsArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub fn run(args: LogsArgs) -> i32 {
    run_cli(|| run_inner(args))
}

fn run_inner(args: LogsArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let lock_path = lock_path(&app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        println!("No dev server lockfile found.");
        return Ok(());
    }

    let lock = read_lock(&lock_path)?;
    debug!(port = lock.port, "Connecting to dev server logs.");
    let response = logs_request(lock.port)?;
    if !response.status().is_success() {
        return Err(format!(
            "Logs request failed with status {}",
            response.status()
        ));
    }

    let mut data_lines: Vec<String> = Vec::new();
    let mut reader = BufReader::new(response);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|err| format!("Failed to read logs: {err}. Raw buffer: {:?}", String::from_utf8_lossy(&buffer)))?;
        if bytes_read == 0 {
            break;
        }
        while buffer.last().is_some_and(|byte| *byte == b'\n' || *byte == b'\r') {
            buffer.pop();
        }
        if buffer.is_empty() {
            if !data_lines.is_empty() {
                let data = data_lines.join("\n");
                data_lines.clear();
                handle_log_payload(&data);
            }
            continue;
        }
        if buffer.starts_with(b"data:") {
            let mut payload = &buffer[5..];
            while payload.first().is_some_and(|byte| *byte == b' ') {
                payload = &payload[1..];
            }
            if payload.is_empty() {
                continue;
            }
            let payload_str = String::from_utf8_lossy(payload);
            if payload_str == "keep-alive" {
                continue;
            }
            data_lines.push(payload_str.to_string());
        }
    }

    if !data_lines.is_empty() {
        let data = data_lines.join("\n");
        handle_log_payload(&data);
    }

    Ok(())
}

fn handle_log_payload(data: &str) {
    match decode_log_payload(data) {
        Ok(payload) => {
            let stream = match payload.stream {
                LogStreamName::App => "app",
                LogStreamName::Ui => "ui",
                LogStreamName::Apx => "apx",
            };
            let pipe = match payload.pipe {
                Some(LogPipe::Out) => "[out] ",
                Some(LogPipe::Error) => "[error] ",
                None => "",
            };
            println!("[{stream}] {pipe}{}", payload.message);
        }
        Err(err) => {
            warn!(error = %err, raw = data, "Failed to parse log payload.");
        }
    }
}
