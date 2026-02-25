use std::path::Path;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::common::{OutputMode, emit};
use crate::dev::client::{HealthCheckConfig, HealthError, status};
use crate::ops::startup_logs::StartupLogStreamer;

/// Wait for dev server to become healthy while streaming logs line-by-line.
pub async fn wait_for_healthy_with_logs(
    port: u16,
    config: &HealthCheckConfig,
    app_dir: &Path,
    mode: OutputMode,
) -> Result<(), String> {
    debug!(
        "Starting health check with config: timeout={}s, retry_delay={}ms, initial_delay={}ms",
        config.timeout_secs, config.retry_delay_ms, config.initial_delay_ms
    );
    tokio::time::sleep(Duration::from_millis(config.initial_delay_ms)).await;

    let start_time = Instant::now();
    let deadline = start_time + Duration::from_secs(config.timeout_secs);
    let mut log_streamer = StartupLogStreamer::new(app_dir, mode).await;
    let mut attempt_count = 0u32;
    let mut last_overall_status: Option<String> = None;
    let mut first_response_logged = false;
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    while Instant::now() < deadline {
        tokio::select! {
            _ = &mut ctrl_c => {
                debug!("Received Ctrl+C, aborting startup");
                return Err("Startup interrupted by user".to_string());
            }
            () = tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)) => {
                log_streamer.print_new_logs().await;
                attempt_count += 1;
                let elapsed_ms = start_time.elapsed().as_millis();

                match status(port).await {
                    Ok(status_response) => {
                        if !first_response_logged {
                            debug!(
                                "Server responding after {}ms (attempt {}) - now waiting for services",
                                elapsed_ms, attempt_count
                            );
                            first_response_logged = true;
                        }

                        if status_response.failed {
                            warn!(
                                "Process failure detected after {}ms - frontend: {}, backend: {}, db: {}",
                                elapsed_ms,
                                status_response.frontend_status,
                                status_response.backend_status,
                                status_response.db_status
                            );
                            return Err(format!(
                                "Process failed and cannot recover. Frontend: {}, Backend: {}, DB: {}",
                                status_response.frontend_status,
                                status_response.backend_status,
                                status_response.db_status
                            ));
                        }

                        if status_response.status == "ok" {
                            debug!(
                                "Health check PASSED on attempt {} after {}ms - services ready (frontend: {}, backend: {}, db: {})",
                                attempt_count,
                                elapsed_ms,
                                status_response.frontend_status,
                                status_response.backend_status,
                                status_response.db_status
                            );

                            if status_response.db_status != "healthy" {
                                emit(mode, "⚠️  Database not available: local development will work but DB features disabled");
                            }

                            return Ok(());
                        }

                        let status_str = format!(
                            "status={}, fe={}, be={}, db={}",
                            status_response.status,
                            status_response.frontend_status,
                            status_response.backend_status,
                            status_response.db_status
                        );

                        let should_log = last_overall_status.as_ref() != Some(&status_str)
                            || attempt_count <= 5
                            || elapsed_ms % 5000 < 250;

                        if should_log {
                            debug!(
                                "Health check attempt {} ({}ms) - {} [waiting for status='ok']",
                                attempt_count, elapsed_ms, status_str
                            );
                        }
                        last_overall_status = Some(status_str);
                    }
                    Err(e) => {
                        let should_log = attempt_count <= 5 || elapsed_ms % 5000 < 250;
                        if should_log {
                            match &e {
                                HealthError::ConnectionFailed(msg) => {
                                    debug!(
                                        "Health check attempt {} ({}ms) - no connection (server not listening yet): {}",
                                        attempt_count, elapsed_ms, msg
                                    );
                                }
                                HealthError::ServerError(msg) => {
                                    warn!(
                                        "Health check attempt {} ({}ms) - server error (server is up but responded with error): {}",
                                        attempt_count, elapsed_ms, msg
                                    );
                                }
                            }
                        }
                        last_overall_status = None;
                    }
                }
            }
        }
    }

    debug!(
        "Health check TIMED OUT after {} attempts ({}ms). Last state: {:?}",
        attempt_count,
        start_time.elapsed().as_millis(),
        last_overall_status
    );

    let detail = match &last_overall_status {
        Some(state) => format!(
            "Dev server failed to become healthy after {}s timeout. Last known state: {state}",
            config.timeout_secs
        ),
        None => format!(
            "Dev server failed to become healthy after {}s timeout (server never responded to health checks)",
            config.timeout_secs
        ),
    };

    Err(detail)
}
