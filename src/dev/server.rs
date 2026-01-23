use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use axum::Router;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::api_generator::start_openapi_watcher;
use crate::dev::common::{lock_path, remove_lock, Shutdown};
use crate::dev::logging::{
    apx_log_queue, apx_log_queue_since, apx_log_queue_since_timestamp, clear_apx_log_queue,
    encode_log_payload, BrowserLogPayload, LogPayload, LogPipe, LogStreamName, SyncLogQueue,
    APX_SHUTDOWN_MESSAGE,
};
use crate::dev::process::ProcessManager;
use crate::dev::proxy;
use crate::dotenv::DotenvFile;
use crate::interop::get_token;

/// Shared application state for the dev server.
#[derive(Clone)]
struct AppState {
    /// Broadcast sender for shutdown signals - the single authority for shutdown coordination.
    shutdown_tx: broadcast::Sender<Shutdown>,
    process_manager: Arc<ProcessManager>,
    apx_log_queue: SyncLogQueue,
}

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    frontend_status: String,
    backend_status: String,
    db_status: String,
}

#[derive(serde::Deserialize)]
struct LogsQuery {
    since: Option<i64>,
    follow: Option<bool>,
}

pub async fn run_server(
    app_dir: PathBuf,
    host: String,
    port: u16,
    backend_port: u16,
    frontend_port: u16,
    db_port: u16,
) -> Result<(), String> {
    let apx_log_queue = apx_log_queue();
    clear_apx_log_queue(&apx_log_queue);

    debug!(
        app_dir = %app_dir.display(),
        host = %host,
        port,
        backend_port,
        frontend_port,
        db_port,
        "Starting dev server."
    );

    // Fetch initial OAuth access token from Python (warn on failure, don't block startup)
    let initial_token = match get_token() {
        Ok(token) => Some(token),
        Err(err) => {
            warn!("Failed to get OAuth access token: {err}. API proxy will not forward authentication headers.");
            None
        }
    };
    let token_manager = Arc::new(proxy::TokenManager::new(initial_token));

    // Create the single shutdown broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<Shutdown>(16);

    let process_manager = Arc::new(
        ProcessManager::start(&app_dir, &host, port, backend_port, frontend_port, db_port).await?,
    );

    // Start .env watcher with shutdown receiver
    start_env_watcher(
        shutdown_tx.subscribe(),
        Arc::clone(&process_manager),
        app_dir.join(".env"),
    );

    // Start OpenAPI watcher with shutdown receiver
    if let Err(err) = start_openapi_watcher(app_dir.clone(), shutdown_tx.subscribe()) {
        warn!("Failed to start OpenAPI watcher: {err}");
    }

    let state = AppState {
        shutdown_tx: shutdown_tx.clone(),
        process_manager: Arc::clone(&process_manager),
        apx_log_queue: Arc::clone(&apx_log_queue),
    };

    // API router - proxied to backend with token manager
    let api_router = proxy::api_router(backend_port, token_manager)?;

    // APX internal router
    let apx_router = Router::new()
        .route("/health", get(health))
        .route("/logs", get(logs).post(browser_logs))
        .route("/stop", get(stop))
        .with_state(state);

    // UI router - proxied to frontend (handles / and /*path)
    let ui_router = proxy::ui_router(frontend_port, process_manager.dev_token())?;

    let app = Router::new()
        .nest("/api", api_router)
        .nest("/_apx", apx_router)
        .merge(ui_router);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|err| format!("Invalid bind address: {err}"))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| format!("Failed to bind server: {err}"))?;

    // Clone what we need for the shutdown handler
    let mut shutdown_rx = shutdown_tx.subscribe();
    let lock = lock_path(&app_dir);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for Stop signal
            match shutdown_rx.recv().await {
                Ok(Shutdown::Stop) => {
                    debug!("Stop signal received, shutting down server.");
                    // ProcessManager owns all process termination
                    process_manager.stop().await;
                    // Remove lock file after processes are stopped
                    let _ = remove_lock(&lock);
                    debug!("Server shutdown complete.");
                }
                Err(_) => {
                    debug!("Shutdown channel closed.");
                }
            }
        })
        .await
        .map_err(|err| format!("Server error: {err}"))?;

    Ok(())
}

/// Start the .env file watcher that restarts uvicorn when environment changes.
fn start_env_watcher(
    mut shutdown_rx: broadcast::Receiver<Shutdown>,
    process_manager: Arc<ProcessManager>,
    dotenv_path: PathBuf,
) {
    tokio::spawn(async move {
        let mut last_vars: HashMap<String, String> = HashMap::new();
        let mut has_loaded = false;

        loop {
            tokio::select! {
                biased;
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(Shutdown::Stop) | Err(_) => {
                            debug!(".env watcher stopping.");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(300)) => {
                    let current_vars = match DotenvFile::read(&dotenv_path) {
                        Ok(dotenv) => dotenv.get_vars(),
                        Err(err) => {
                            warn!("Failed to read .env: {err}");
                            continue;
                        }
                    };
                    if has_loaded && current_vars != last_vars {
                        info!(".env changed, restarting uvicorn");
                        if let Err(err) = process_manager
                            .restart_uvicorn_with_env(current_vars.clone())
                            .await
                        {
                            warn!("Failed to restart uvicorn: {err}");
                        }
                    }
                    last_vars = current_vars;
                    has_loaded = true;
                }
            }
        }
    });
}

async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let (frontend_status, backend_status, db_status) = state.process_manager.status().await;
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            frontend_status,
            backend_status,
            db_status,
        }),
    )
}

async fn logs(
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> axum::response::Response {
    let since = query.since.unwrap_or(0);
    let follow = query.follow.unwrap_or(false);
    let (tx, rx) = mpsc::channel(200);
    let mut shutdown_rx = state.shutdown_tx.subscribe();

    tokio::spawn(async move {
        // Send initial logs (merged and sorted by timestamp)
        let (mut app_len, initial_logs) = state.process_manager.logs_since_timestamp(since).await;
        let (mut apx_len, initial_apx_logs) =
            apx_log_queue_since_timestamp(&state.apx_log_queue, since);

        let mut merged = initial_logs;
        merged.extend(initial_apx_logs);
        merged.sort_by_key(|log| log.timestamp);

        for entry in merged {
            if send_log_payload(&tx, entry).await.is_err() {
                return;
            }
        }

        if !follow {
            return;
        }

        // Follow mode: poll logs until shutdown signal
        let poll_interval = Duration::from_millis(100);

        loop {
            tokio::select! {
                biased;
                // React to shutdown signal
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(Shutdown::Stop) => {
                            // Flush any remaining logs before sending shutdown message (merged and sorted)
                            let (_, final_logs) = state.process_manager.logs_since_index(app_len).await;
                            let (_, final_apx_logs) = apx_log_queue_since(&state.apx_log_queue, apx_len);

                            let mut merged = final_logs;
                            merged.extend(final_apx_logs);
                            merged.sort_by_key(|log| log.timestamp);

                            for entry in merged {
                                let _ = send_log_payload(&tx, entry).await;
                            }

                            // Send final shutdown message and close
                            let _ = send_log_payload(
                                &tx,
                                LogPayload::new(
                                    LogStreamName::Apx,
                                    None,
                                    APX_SHUTDOWN_MESSAGE.to_string(),
                                ),
                            )
                            .await;
                            return;
                        }
                        Err(_) => {
                            // Channel closed
                            return;
                        }
                    }
                }
                // Poll for new logs (merged and sorted)
                _ = tokio::time::sleep(poll_interval) => {
                    let (next_app_len, logs) = state.process_manager.logs_since_index(app_len).await;
                    let (next_apx_len, apx_logs) = apx_log_queue_since(&state.apx_log_queue, apx_len);

                    let mut merged = logs;
                    merged.extend(apx_logs);
                    merged.sort_by_key(|log| log.timestamp);

                    for entry in merged {
                        if send_log_payload(&tx, entry).await.is_err() {
                            return;
                        }
                    }

                    app_len = next_app_len;
                    apx_len = next_apx_len;
                }
            }
        }
    });

    if follow {
        Sse::new(ReceiverStream::new(rx))
            .keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(10))
                    .text("keep-alive"),
            )
            .into_response()
    } else {
        Sse::new(ReceiverStream::new(rx)).into_response()
    }
}

async fn browser_logs(
    State(state): State<AppState>,
    Json(payload): Json<BrowserLogPayload>,
) -> StatusCode {
    let level = payload.level.as_str();
    let pipe = if level == "error" {
        LogPipe::Error
    } else {
        LogPipe::Out
    };
    let mut message = format!(
        "[browser:{}:{}] {}",
        level,
        payload.source,
        payload.message
    );
    if let Some(stack) = payload.stack {
        message.push('\n');
        message.push_str(&stack);
    }
    let log_payload = LogPayload {
        stream: LogStreamName::Ui,
        pipe: Some(pipe),
        message,
        timestamp: payload.timestamp,
    };
    state.process_manager.push_browser_log(log_payload).await;
    StatusCode::OK
}

async fn stop(State(state): State<AppState>) -> StatusCode {
    debug!("Received dev server stop request.");
    // Send the shutdown signal - all subscribers will react appropriately
    let _ = state.shutdown_tx.send(Shutdown::Stop);
    StatusCode::OK
}

async fn send_log_payload(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    payload: LogPayload,
) -> Result<(), ()> {
    let data = encode_log_payload(&payload).map_err(|_| ())?;
    tx.send(Ok(Event::default().data(data))).await.map_err(|_| ())
}

