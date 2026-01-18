use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;
use axum::Router;
use std::convert::Infallible;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

use crate::dev::common::{lock_path, remove_lock};
use crate::dev::logging::{
    apx_log_queue, apx_log_queue_since, apx_log_queue_since_timestamp, clear_apx_log_queue,
    encode_log_payload, BrowserLogPayload, LogPayload, LogPipe, LogStreamName, SyncLogQueue,
    APX_SHUTDOWN_MESSAGE,
};
use crate::dev::proxy;
use crate::dev::process::ProcessManager;
use crate::api_generator::start_openapi_watcher;
use crate::dotenv::DotenvFile;

#[derive(Clone)]
struct AppState {
    shutdown_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    app_dir: PathBuf,
    process_manager: Arc<ProcessManager>,
    apx_log_queue: SyncLogQueue,
    is_stopping: Arc<AtomicBool>,
    shutdown_message_sent: Arc<AtomicBool>,
}

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    frontend_status: String,
    backend_status: String,
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
) -> Result<(), String> {
    let apx_log_queue = apx_log_queue();
    clear_apx_log_queue(&apx_log_queue);

    debug!(
        app_dir = %app_dir.display(),
        host = %host,
        port,
        backend_port,
        frontend_port,
        "Starting dev server."
    );
    let process_manager = Arc::new(
        ProcessManager::start(&app_dir, &host, port, backend_port, frontend_port).await?,
    );
    let is_stopping = Arc::new(AtomicBool::new(false));
    let shutdown_message_sent = Arc::new(AtomicBool::new(false));

    {
        let dotenv_path = app_dir.join(".env");
        let process_manager = Arc::clone(&process_manager);
        let is_stopping = Arc::clone(&is_stopping);
        tokio::spawn(async move {
            let mut last_vars: HashMap<String, String> = HashMap::new();
            let mut has_loaded = false;
            loop {
                if is_stopping.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
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
        });
    }
    if let Err(err) = start_openapi_watcher(app_dir.clone(), Arc::clone(&is_stopping)) {
        warn!("Failed to start OpenAPI watcher: {err}");
    }
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = AppState {
        shutdown_tx: Arc::new(tokio::sync::Mutex::new(Some(shutdown_tx))),
        app_dir,
        process_manager: Arc::clone(&process_manager),
        apx_log_queue: Arc::clone(&apx_log_queue),
        is_stopping: Arc::clone(&is_stopping),
        shutdown_message_sent: Arc::clone(&shutdown_message_sent),
    };

    // API router - proxied to backend
    let api_router = proxy::api_router(backend_port)?;

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

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for the shutdown signal first - code after await doesn't run until signal received
            let _ = shutdown_rx.await;
            debug!("Stopping the server and its subprocesses.");
            process_manager.stop().await;
            debug!("Server and subprocesses stopped.");
        })
        .await
        .map_err(|err| format!("Server error: {err}"))?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let (frontend_status, backend_status) = state.process_manager.status().await;
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            frontend_status,
            backend_status,
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
    let state = state.clone();
    tokio::spawn(async move {
        let (mut app_len, initial_logs) = state.process_manager.logs_since_timestamp(since).await;
        let (mut apx_len, initial_apx_logs) =
            apx_log_queue_since_timestamp(&state.apx_log_queue, since);
        for entry in initial_logs {
            if send_log_payload(&tx, entry).await.is_err() {
                return;
            }
        }
        for message in initial_apx_logs {
            if send_log_payload(&tx, message).await.is_err() {
                return;
            }
        }
        if !follow {
            return;
        }
        let mut shutdown_initiated = false;
        loop {
            // Poll more frequently when stopping to catch shutdown quickly
            let poll_interval = if state.is_stopping.load(Ordering::SeqCst) {
                Duration::from_millis(10)
            } else {
                Duration::from_millis(100)
            };
            tokio::time::sleep(poll_interval).await;

            // Check if stopping was initiated
            if state.is_stopping.load(Ordering::SeqCst) && !shutdown_initiated {
                shutdown_initiated = true;
                // Wait briefly for any final logs to be generated
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            let (next_app_len, logs) = state.process_manager.logs_since_index(app_len).await;
            let (next_apx_len, apx_logs) = apx_log_queue_since(&state.apx_log_queue, apx_len);
            for entry in logs {
                if send_log_payload(&tx, entry).await.is_err() {
                    return;
                }
            }
            for message in apx_logs {
                if send_log_payload(&tx, message).await.is_err() {
                    return;
                }
            }
            app_len = next_app_len;
            apx_len = next_apx_len;

            // Once stopping and processes complete, send final message and exit
            if shutdown_initiated && state.process_manager.is_shutdown_complete().await {
                if state.shutdown_message_sent.load(Ordering::SeqCst) {
                    return;
                }
                // Flush any remaining logs
                let (_, final_logs) = state.process_manager.logs_since_index(app_len).await;
                let (_, final_apx_logs) = apx_log_queue_since(&state.apx_log_queue, apx_len);
                for entry in final_logs {
                    let _ = send_log_payload(&tx, entry).await;
                }
                for message in final_apx_logs {
                    let _ = send_log_payload(&tx, message).await;
                }
                // Send shutdown message
                if send_log_payload(
                    &tx,
                    LogPayload::new(
                        LogStreamName::Apx,
                        None,
                        APX_SHUTDOWN_MESSAGE.to_string(),
                    ),
                )
                .await
                .is_ok()
                {
                    state.shutdown_message_sent.store(true, Ordering::SeqCst);
                    // Give time for the message to be transmitted before closing
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                return;
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
    state.is_stopping.store(true, Ordering::SeqCst);
    let lock = lock_path(&state.app_dir);
    let _ = remove_lock(&lock);

    // Stop the processes
    state.process_manager.stop().await;
    debug!("Processes stopped, waiting for shutdown message to be sent.");

    // Wait for the logs stream to send the shutdown message (up to 3 seconds)
    let mut wait_attempts = 0;
    while !state.shutdown_message_sent.load(Ordering::SeqCst) && wait_attempts < 60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        wait_attempts += 1;
    }

    if state.shutdown_message_sent.load(Ordering::SeqCst) {
        debug!("Shutdown message sent, giving time for transmission.");
        // Give extra time for the message to be transmitted to clients
        tokio::time::sleep(Duration::from_millis(100)).await;
    } else {
        debug!("Shutdown message was not sent within timeout.");
    }

    let mut guard = state.shutdown_tx.lock().await;
    if let Some(tx) = guard.take() {
        debug!("Dispatching dev server shutdown signal.");
        let _ = tx.send(());
    }

    StatusCode::OK
}

async fn send_log_payload(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    payload: LogPayload,
) -> Result<(), ()> {
    let data = encode_log_payload(&payload).map_err(|_| ())?;
    tx.send(Ok(Event::default().data(data))).await.map_err(|_| ())
}

