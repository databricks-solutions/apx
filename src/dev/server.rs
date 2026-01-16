use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::Json;
use axum::Router;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;

use crate::dev::common::{lock_path, remove_lock};
use crate::dev::logging::{
    drain_log_queue, encode_log_payload, is_log_queue_empty, new_log_queue, set_apx_log_queue,
    LogPayload, LogQueue, LogStreamName,
};
use crate::dev::process::ProcessManager;

#[derive(Clone)]
struct AppState {
    shutdown_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    app_dir: PathBuf,
    process_manager: Arc<ProcessManager>,
    apx_log_queue: LogQueue,
    is_stopping: Arc<AtomicBool>,
}

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    frontend_status: String,
    backend_status: String,
}

pub async fn run_server(
    app_dir: PathBuf,
    host: String,
    port: u16,
    backend_port: u16,
    frontend_port: u16,
) -> Result<(), String> {
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
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let apx_log_queue = new_log_queue();
    set_apx_log_queue(Arc::clone(&apx_log_queue));
    let is_stopping = Arc::new(AtomicBool::new(false));
    let state = AppState {
        shutdown_tx: Arc::new(tokio::sync::Mutex::new(Some(shutdown_tx))),
        app_dir,
        process_manager: Arc::clone(&process_manager),
        apx_log_queue: Arc::clone(&apx_log_queue),
        is_stopping: Arc::clone(&is_stopping),
    };

    let app = Router::new()
        .route("/_apx/health", get(health))
        .route("/_apx/logs", get(logs))
        .route("/_apx/stop", get(stop))
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|err| format!("Invalid bind address: {err}"))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| format!("Failed to bind server: {err}"))?;

    let is_stopping_shutdown = Arc::clone(&is_stopping);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            debug!("Dev server shutdown signal received.");
            is_stopping_shutdown.store(true, Ordering::SeqCst);
            let _ = shutdown_rx.await;
            process_manager.stop().await;
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

async fn logs(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    let (tx, rx) = mpsc::channel(200);
    let state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            let logs = state.process_manager.drain_logs().await;
            let apx_logs = drain_log_queue(&state.apx_log_queue).await;
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

            if state.is_stopping.load(Ordering::SeqCst)
                && state.process_manager.is_shutdown_complete().await
                && state.process_manager.is_log_queue_empty().await
                && is_log_queue_empty(&state.apx_log_queue).await
            {
                let _ = send_log_payload(
                    &tx,
                    LogPayload {
                        stream: LogStreamName::Apx,
                        pipe: None,
                        message: "Dev server shutdown complete.".to_string(),
                    },
                )
                .await;
                return;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("keep-alive"),
    )
}

async fn stop(State(state): State<AppState>) -> StatusCode {
    debug!("Received dev server stop request.");
    state.is_stopping.store(true, Ordering::SeqCst);
    let lock = lock_path(&state.app_dir);
    let _ = remove_lock(&lock);

    state.process_manager.stop().await;

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
