//! APX dev server with flux-based logging.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::Duration;
use tracing::{debug, info, warn};

use apx_databricks_sdk::DatabricksClient;

use crate::api_generator::start_openapi_watcher;
use crate::dev::common::{Shutdown, lock_path, remove_lock};
use crate::dev::logging::BrowserLogPayload;
use crate::dev::otel::build_otlp_log_payload_from_ms;
use crate::dev::process::ProcessManager;
use crate::dev::proxy;
use crate::dotenv::DotenvFile;
use crate::flux;

/// Shared application state for the dev server.
#[derive(Clone)]
struct AppState {
    /// Broadcast sender for shutdown signals - the single authority for shutdown coordination.
    shutdown_tx: broadcast::Sender<Shutdown>,
    process_manager: Arc<ProcessManager>,
    /// HTTP client for forwarding browser logs to flux
    http_client: reqwest::Client,
    /// App directory path for resource attributes
    app_dir: PathBuf,
}

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    frontend_status: String,
    backend_status: String,
    db_status: String,
    /// True if any critical process (frontend/backend) has permanently failed and cannot recover
    failed: bool,
}

/// Run the dev server with a pre-bound listener.
/// The listener is passed in to avoid TOCTOU race conditions with port allocation.
pub async fn run_server(
    app_dir: PathBuf,
    listener: tokio::net::TcpListener,
    backend_port: u16,
    frontend_port: Option<u16>,
    db_port: u16,
) -> Result<(), String> {
    // Ensure flux is running for log collection
    if let Err(e) = flux::ensure_running() {
        warn!(
            "Failed to start flux: {}. Logging may not work correctly.",
            e
        );
    }

    // Extract port and host from the pre-bound listener
    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to get listener address: {e}"))?;
    let port = local_addr.port();
    let host = local_addr.ip().to_string();

    debug!(
        app_dir = %app_dir.display(),
        host = %host,
        port,
        backend_port,
        frontend_port = ?frontend_port,
        db_port,
        "Starting dev server."
    );

    // Resolve Databricks profile from env or .env file
    let profile = resolve_databricks_profile(&app_dir);
    let databricks_client = match &profile {
        Some(p) => {
            match DatabricksClient::with_product(p, "apx", env!("CARGO_PKG_VERSION")).await {
                Ok(client) => Some(client),
                Err(err) => {
                    warn!(
                        "Failed to create Databricks client: {err}. API proxy will not forward authentication headers."
                    );
                    None
                }
            }
        }
        None => {
            warn!(
                "No Databricks profile configured. API proxy will not forward authentication headers."
            );
            None
        }
    };

    // Compute forwarded user header once at startup
    let forwarded_user_header = match &databricks_client {
        Some(client) => match apx_databricks_sdk::get_forwarded_user_header(client.profile()).await
        {
            Ok(header) => Some(header),
            Err(err) => {
                warn!(error = %err, "Failed to get forwarded user header");
                None
            }
        },
        None => None,
    };

    let token_manager = Arc::new(proxy::TokenManager::new(databricks_client));

    // Create the single shutdown broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<Shutdown>(16);

    // Create ProcessManager (doesn't spawn processes yet)
    let process_manager = Arc::new(ProcessManager::new(
        &app_dir,
        &host,
        port,
        backend_port,
        frontend_port,
        db_port,
    )?);

    // Spawn processes in background (DB → Vite → Uvicorn)
    // This returns immediately - health endpoint will report status as processes come up
    process_manager.start_processes();
    debug!("Process spawning started in background");

    // Start .env watcher with shutdown receiver
    start_env_watcher(
        shutdown_tx.subscribe(),
        Arc::clone(&process_manager),
        app_dir.join(".env"),
    );

    // Start OpenAPI watcher with shutdown receiver (only for projects with UI)
    if process_manager.has_ui()
        && let Err(err) = start_openapi_watcher(app_dir.clone(), shutdown_tx.subscribe())
    {
        warn!("Failed to start OpenAPI watcher: {err}");
    }

    // Start filesystem watcher to stop server if project folder or lock file is removed
    start_filesystem_watcher(
        shutdown_tx.subscribe(),
        shutdown_tx.clone(),
        app_dir.clone(),
    );

    // Create HTTP client for OTLP forwarding
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let state = AppState {
        shutdown_tx: shutdown_tx.clone(),
        process_manager: Arc::clone(&process_manager),
        http_client,
        app_dir: app_dir.clone(),
    };

    // API router - proxied to backend with token manager
    let api_router = proxy::api_router(
        backend_port,
        Arc::clone(&token_manager),
        forwarded_user_header.clone(),
    )?;

    // API utilities router - proxied to backend for FastAPI docs (/docs, /redoc, /openapi.json)
    let api_utils_router =
        proxy::api_utils_router(backend_port, token_manager, forwarded_user_header)?;

    // APX internal router
    let apx_router = Router::new()
        .route("/health", get(health))
        .route("/logs", axum::routing::post(browser_logs))
        .route("/stop", get(stop))
        .with_state(state);

    let base_router = Router::new()
        .nest("/api", api_router)
        .nest("/_apx", apx_router)
        .merge(api_utils_router);

    // UI router - proxied to frontend (handles / and /*path), only for projects with UI
    let app = if let Some(fp) = frontend_port {
        let ui_router = proxy::ui_router(fp, process_manager.dev_token())?;
        base_router.merge(ui_router)
    } else {
        base_router
    };

    // Clone what we need for the shutdown handler
    let mut shutdown_rx = shutdown_tx.subscribe();
    let lock = lock_path(&app_dir);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for Stop signal
            match shutdown_rx.recv().await {
                Ok(Shutdown::Stop) => {
                    debug!("Stop signal received, shutting down server.");
                    // Give process_manager.stop() a hard deadline to prevent indefinite hangs
                    match tokio::time::timeout(Duration::from_secs(10), process_manager.stop())
                        .await
                    {
                        Ok(()) => debug!("Process shutdown complete."),
                        Err(_) => warn!("Process shutdown timed out after 10s, forcing exit."),
                    }

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

/// Start the filesystem watcher that stops the server if the project folder
/// or the lock file is removed.
fn start_filesystem_watcher(
    mut shutdown_rx: broadcast::Receiver<Shutdown>,
    shutdown_tx: broadcast::Sender<Shutdown>,
    app_dir: PathBuf,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(Shutdown::Stop) | Err(_) => {
                            debug!("Filesystem watcher stopping.");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    // Check if project folder was removed
                    if !app_dir.exists() {
                        warn!(
                            "Project folder '{}' was removed, stopping dev server.",
                            app_dir.display()
                        );
                        let _ = shutdown_tx.send(Shutdown::Stop);
                        break;
                    }
                }
            }
        }
    });
}

async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let (frontend_status, backend_status, db_status) = state.process_manager.status().await;
    let has_ui = state.process_manager.has_ui();

    // Check if any critical process has permanently failed (crashed/exited)
    let failed = if has_ui {
        frontend_status == "failed" || backend_status == "failed"
    } else {
        backend_status == "failed"
    };

    // DB is non-critical - only critical services must be healthy for "ok" status
    let all_healthy = if has_ui {
        frontend_status == "healthy" && backend_status == "healthy"
    } else {
        // Backend-only: only backend needs to be healthy
        backend_status == "healthy"
    };
    let status = if all_healthy { "ok" } else { "starting" };

    (
        StatusCode::OK,
        Json(HealthResponse {
            status,
            frontend_status,
            backend_status,
            db_status, // Reported but doesn't affect overall status
            failed,
        }),
    )
}

async fn browser_logs(
    State(state): State<AppState>,
    Json(payload): Json<BrowserLogPayload>,
) -> StatusCode {
    let mut message = format!(
        "[browser:{}:{}] {}",
        payload.level, payload.source, payload.message
    );
    if let Some(stack) = payload.stack {
        message.push('\n');
        message.push_str(&stack);
    }

    // Forward to flux via OTLP HTTP using shared otel module
    let otlp_payload = build_otlp_log_payload_from_ms(
        &message,
        &payload.level,
        payload.timestamp,
        "browser",
        &state.app_dir,
    );

    let endpoint = format!("http://127.0.0.1:{}/v1/logs", flux::FLUX_PORT);
    let result = state
        .http_client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&otlp_payload)
        .send()
        .await;

    if let Err(e) = result {
        debug!("Failed to forward browser log to flux: {}", e);
    }

    StatusCode::OK
}

async fn stop(State(state): State<AppState>) -> StatusCode {
    debug!("Received dev server stop request.");

    // Send the shutdown signal
    let _ = state.shutdown_tx.send(Shutdown::Stop);
    StatusCode::OK
}

/// Resolve the Databricks profile name from env var or `.env` file.
fn resolve_databricks_profile(app_dir: &std::path::Path) -> Option<String> {
    std::env::var("DATABRICKS_CONFIG_PROFILE").ok().or_else(|| {
        DotenvFile::read(&app_dir.join(".env"))
            .ok()
            .and_then(|d| d.get_vars().get("DATABRICKS_CONFIG_PROFILE").cloned())
    })
}
