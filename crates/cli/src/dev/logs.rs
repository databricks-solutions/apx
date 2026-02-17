//! Log viewer for APX dev server using flux SQLite storage.
//!
//! Reads logs from ~/.apx/logs/db which is maintained by flux.

use clap::Args;
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;

use crate::common::resolve_app_dir;
use crate::run_cli_async_helper;
use apx_common::{LogAggregator, should_skip_log};
use apx_core::dev::common::{lock_path, read_lock};
use apx_core::ops::logs::{
    DEFAULT_LOG_DURATION, format_aggregated_record, format_log_record, parse_duration,
    since_timestamp_nanos,
};
use apx_db::LogsDb;

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

pub async fn run(args: LogsArgs) -> i32 {
    run_cli_async_helper(|| run_async(args)).await
}

async fn run_async(args: LogsArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path.clone());

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
    let db_path = apx_db::logs_db_path()?;
    if !db_path.exists() {
        println!("⚠️  No logs database found at {}\n", db_path.display());
        println!("Logs will appear here once the dev server is started and produces output.");
        return Ok(());
    }

    // Open storage
    let storage = LogsDb::open()
        .await
        .map_err(|e| format!("Failed to open logs database: {e}"))?;

    let duration = parse_duration(&args.duration)?;
    let since_ns = since_timestamp_nanos(duration);

    if args.follow {
        println!("📜 Streaming logs... (Ctrl+C to stop)\n");
        follow_logs(&storage, &app_path_canonical, since_ns, &lock_path).await
    } else {
        read_logs(&storage, &app_path_canonical, since_ns).await
    }
}

/// Read logs from database, filtered by app path and timestamp
async fn read_logs(storage: &LogsDb, app_path: &str, since_ns: i64) -> Result<(), String> {
    let records = storage.query_logs(Some(app_path), since_ns, None).await?;

    let filtered: Vec<_> = records.iter().filter(|r| !should_skip_log(r)).collect();

    if filtered.is_empty() {
        println!("No logs found for the specified time range.");
        return Ok(());
    }

    // Use aggregator for repetitive messages
    let mut aggregator = LogAggregator::new();

    for record in &filtered {
        let timestamp_ms = record.effective_timestamp_ms();

        // Flush expired aggregations before processing this record
        for agg in aggregator.flush_expired(timestamp_ms) {
            println!("{}", format_aggregated_record(&agg, true));
        }

        // Try to aggregate, if not aggregatable print directly
        if !aggregator.add(record) {
            println!("{}", format_log_record(record, true));
        }
    }

    // Flush any remaining aggregations
    for agg in aggregator.flush_all() {
        println!("{}", format_aggregated_record(&agg, true));
    }

    Ok(())
}

/// Follow logs for new entries
async fn follow_logs(
    storage: &LogsDb,
    app_path: &str,
    since_ns: i64,
    lock_path: &std::path::Path,
) -> Result<(), String> {
    use chrono::Utc;

    // First, read existing logs
    read_logs(storage, app_path, since_ns).await?;

    // Track last seen ID for incremental queries
    let mut last_id = storage.get_latest_id().await?;

    // Track if server was initially running
    let server_was_running = lock_path.exists();

    // Aggregator for follow mode
    let mut aggregator = LogAggregator::new();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                debug!("Received Ctrl+C, stopping logs stream.");
                // Flush remaining aggregations
                for agg in aggregator.flush_all() {
                    println!("{}", format_aggregated_record(&agg, true));
                }
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                let current_time_ms = Utc::now().timestamp_millis();

                // Flush expired aggregations
                for agg in aggregator.flush_expired(current_time_ms) {
                    println!("{}", format_aggregated_record(&agg, true));
                }

                // Poll for new logs
                let new_records = storage.query_logs_after_id(Some(app_path), last_id).await?;

                for record in &new_records {
                    if !should_skip_log(record) {
                        // Try to aggregate, if not aggregatable print directly
                        if !aggregator.add(record) {
                            println!("{}", format_log_record(record, true));
                        }
                    }
                }

                // Update last_id
                if let Ok(new_id) = storage.get_latest_id().await
                    && new_id > last_id
                {
                    last_id = new_id;
                }

                // Check if server was running but lockfile is now gone
                if server_was_running && !lock_path.exists() {
                    debug!("Dev server stopped (lockfile removed), exiting logs follow.");
                    // Flush remaining aggregations
                    for agg in aggregator.flush_all() {
                        println!("{}", format_aggregated_record(&agg, true));
                    }
                    println!("\n📭 Dev server stopped.");
                    break;
                }
            }
        }
    }

    Ok(())
}
