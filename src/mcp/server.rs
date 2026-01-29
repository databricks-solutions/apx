use crate::cli::components::{SharedCacheState, needs_registry_refresh, sync_registry_indexes};
use crate::common::read_project_metadata;
use crate::databricks_sdk_doc::SDKSource;
use crate::dotenv::DotenvFile;
use crate::interop::generate_openapi_spec;
use crate::mcp::core::{McpServer, ToolResult};
use crate::search::ComponentIndex;
use crate::search::docs_index::SDKDocsIndex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, broadcast};

/// Parameters for SDK indexing, pre-computed synchronously to avoid Python GIL issues
pub struct SdkIndexParams {
    pub sdk_version: Option<String>,
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
}

/// State for tracking index readiness
#[derive(Clone)]
pub struct IndexState {
    /// Notifies waiters when component index is ready
    pub component_ready: Arc<Notify>,
    /// Notifies waiters when SDK docs index is ready
    pub sdk_ready: Arc<Notify>,
    /// Whether component indexing has completed (for late subscribers)
    pub component_indexed: Arc<AtomicBool>,
    /// Whether SDK indexing has completed (for late subscribers)
    pub sdk_indexed: Arc<AtomicBool>,
}

impl IndexState {
    pub fn new() -> Self {
        Self {
            component_ready: Arc::new(Notify::new()),
            sdk_ready: Arc::new(Notify::new()),
            component_indexed: Arc::new(AtomicBool::new(false)),
            sdk_indexed: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub struct AppContext {
    pub app_dir: PathBuf,
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
    pub cache_state: SharedCacheState,
    pub index_state: IndexState,
    pub shutdown_tx: broadcast::Sender<()>,
}

/// Initialize all indexes in background (component index, then SDK docs index)
///
/// All database operations are done sequentially in a single task to avoid
/// Lance's internal task conflicts when multiple connections access the database.
///
/// This is called when the MCP server starts.
pub fn init_all_indexes(
    ctx: &AppContext,
    mut shutdown_rx: broadcast::Receiver<()>,
    sdk_params: Option<SdkIndexParams>,
) {
    let app_dir = ctx.app_dir.clone();
    let cache_state = ctx.cache_state.clone();
    let index_state = ctx.index_state.clone();

    tokio::spawn(async move {
        // Mark as running
        {
            let mut guard = cache_state.lock().await;
            guard.is_running = true;
        }

        // ============================================
        // Phase 1: Component Index
        // ============================================
        tracing::info!("Syncing registry indexes on MCP start");

        // Check for shutdown during sync
        let sync_result = tokio::select! {
            result = sync_registry_indexes(&app_dir, false) => Some(result),
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received during registry sync, stopping");
                None
            }
        };

        match sync_result {
            Some(Ok(refreshed)) => {
                if refreshed {
                    tracing::info!("Registry indexes refreshed, rebuilding search index");

                    // Check for shutdown during rebuild
                    let rebuild_result = tokio::select! {
                        result = rebuild_search_index() => Some(result),
                        _ = shutdown_rx.recv() => {
                            tracing::info!("Shutdown signal received during search index rebuild, stopping");
                            None
                        }
                    };

                    if let Some(Err(e)) = rebuild_result {
                        tracing::warn!("Failed to rebuild search index: {}", e);
                    }
                } else {
                    // Check if search index exists, build if not
                    let ensure_result = tokio::select! {
                        result = ensure_search_index() => Some(result),
                        _ = shutdown_rx.recv() => {
                            tracing::info!("Shutdown signal received during search index check, stopping");
                            None
                        }
                    };

                    if let Some(Err(e)) = ensure_result {
                        tracing::warn!("Failed to ensure search index: {}", e);
                    }
                }
            }
            Some(Err(e)) => tracing::warn!("Failed to sync registry indexes: {}", e),
            None => {
                // Shutdown was signaled during sync
            }
        }

        // Mark component indexing as complete
        index_state.component_indexed.store(true, Ordering::SeqCst);
        index_state.component_ready.notify_waiters();
        tracing::debug!("Component index ready");

        // ============================================
        // Phase 2: SDK Docs Index (after component index)
        // ============================================
        if let Some(params) = sdk_params {
            tracing::info!("Initializing Databricks SDK documentation index");

            let version = match params.sdk_version {
                Some(v) => {
                    tracing::debug!("Using pre-computed SDK version: {}", v);
                    v
                }
                None => {
                    tracing::warn!(
                        "Databricks SDK not installed. The docs tool will not be available."
                    );
                    index_state.sdk_indexed.store(true, Ordering::SeqCst);
                    index_state.sdk_ready.notify_waiters();

                    // Mark as done
                    let mut guard = cache_state.lock().await;
                    guard.is_running = false;
                    return;
                }
            };

            // Create SDK docs index
            let index_result = tokio::select! {
                result = async { SDKDocsIndex::new() } => Some(result),
                _ = shutdown_rx.recv() => {
                    tracing::info!("Shutdown signal received during SDK doc index initialization");
                    None
                }
            };

            let mut index = match index_result {
                Some(Ok(idx)) => {
                    tracing::debug!("SDKDocsIndex created successfully");
                    idx
                }
                Some(Err(e)) => {
                    tracing::warn!(
                        "Failed to initialize SDK doc index: {}. The docs tool will not be available.",
                        e
                    );
                    index_state.sdk_indexed.store(true, Ordering::SeqCst);
                    index_state.sdk_ready.notify_waiters();

                    let mut guard = cache_state.lock().await;
                    guard.is_running = false;
                    return;
                }
                None => {
                    index_state.sdk_indexed.store(true, Ordering::SeqCst);
                    index_state.sdk_ready.notify_waiters();

                    let mut guard = cache_state.lock().await;
                    guard.is_running = false;
                    return;
                }
            };

            // Bootstrap the index
            tracing::info!("Bootstrapping SDK docs (this may download SDK if not cached)");
            let bootstrap_start = std::time::Instant::now();
            let bootstrap_result = tokio::select! {
                result = index.bootstrap_with_version(&SDKSource::DatabricksSdkPython, &version) => Some(result),
                _ = shutdown_rx.recv() => {
                    tracing::info!("Shutdown signal received during SDK doc bootstrapping");
                    None
                }
            };
            tracing::debug!("SDK bootstrap completed in {:?}", bootstrap_start.elapsed());

            match bootstrap_result {
                Some(Ok(true)) => {
                    tracing::info!("SDK docs indexed successfully");
                    *params.sdk_doc_index.lock().await = Some(index);
                }
                Some(Ok(false)) => {
                    tracing::info!("SDK docs already indexed");
                    *params.sdk_doc_index.lock().await = Some(index);
                }
                Some(Err(e)) => {
                    tracing::warn!(
                        "Failed to bootstrap SDK docs: {}. The docs tool will not be available.",
                        e
                    );
                }
                None => {
                    tracing::debug!("Shutdown during SDK bootstrap");
                }
            }

            // Mark SDK indexing as complete
            index_state.sdk_indexed.store(true, Ordering::SeqCst);
            index_state.sdk_ready.notify_waiters();
            tracing::debug!("SDK doc index ready");
        } else {
            // No SDK params, mark as ready immediately
            index_state.sdk_indexed.store(true, Ordering::SeqCst);
            index_state.sdk_ready.notify_waiters();
        }

        // Mark as done
        {
            let mut guard = cache_state.lock().await;
            guard.is_running = false;
        }
    });
}

/// Rebuild the search index from registry.json files
async fn rebuild_search_index() -> Result<(), String> {
    let db_path = ComponentIndex::default_path()?;
    let index = ComponentIndex::new(db_path)?;
    let table_name = ComponentIndex::table_name("components");

    index.build_index_from_registries(&table_name).await
}

/// Ensure search index exists and is valid, build/rebuild if needed
async fn ensure_search_index() -> Result<(), String> {
    let db_path = ComponentIndex::default_path()?;
    let index = ComponentIndex::new(db_path)?;
    let table_name = ComponentIndex::table_name("components");

    // Validate the index - this checks both existence and data integrity
    match index.validate_index(&table_name).await {
        Ok(true) => {
            tracing::debug!("Search index validated successfully");
            Ok(())
        }
        Ok(false) => {
            // Index doesn't exist, build it
            tracing::info!("Search index not found, building from registry indexes");
            index.build_index_from_registries(&table_name).await
        }
        Err(e) => {
            // Index is corrupted, rebuild it
            tracing::warn!("Search index corrupted ({}), rebuilding...", e);
            index.build_index_from_registries(&table_name).await
        }
    }
}

/// Wait for an index to be ready with timeout (15 seconds)
async fn wait_for_index_ready(
    ready_notify: &Notify,
    is_ready: &AtomicBool,
    index_name: &str,
) -> Result<(), String> {
    const TIMEOUT_SECS: u64 = 15;

    // Check if already ready
    if is_ready.load(Ordering::SeqCst) {
        return Ok(());
    }

    tracing::debug!(
        "Waiting up to {}s for {} index to be ready",
        TIMEOUT_SECS,
        index_name
    );

    // Wait with timeout
    match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), ready_notify.notified()).await {
        Ok(_) => {
            tracing::debug!("{} index is now ready", index_name);
            Ok(())
        }
        Err(_) => {
            tracing::warn!(
                "{} index not ready after {}s timeout",
                index_name,
                TIMEOUT_SECS
            );
            Err(format!(
                "{index_name} index is not yet ready, please rerun the query in 5 seconds"
            ))
        }
    }
}

pub fn build_server(ctx: AppContext, sdk_params: Option<SdkIndexParams>) -> McpServer<AppContext> {
    // Initialize all indexes in background (component + SDK docs, sequentially)
    let shutdown_rx = ctx.shutdown_tx.subscribe();
    init_all_indexes(&ctx, shutdown_rx, sdk_params);

    McpServer::new(ctx)
        .resource(
            "apx://info",
            "apx-info",
            "Information about apx toolkit",
            "text/plain",
            apx_info_resource,
        )
        .resource(
            "apx://routes",
            "api-routes",
            "List of API routes from OpenAPI schema",
            "application/json",
            routes_resource,
        )
        .tool(
            "start",
            "Start development server and return the URL",
            start_tool,
        )
        .tool(
            "stop",
            "Stop the development server",
            stop_tool,
        )
        .tool(
            "restart",
            "Restart the development server (preserves port if possible)",
            restart_tool,
        )
        .tool(
            "logs",
            "Fetch recent dev server logs",
            logs_tool,
        )
        .tool(
            "refresh_openapi",
            "Regenerate OpenAPI schema and API client",
            refresh_openapi_tool,
        )
        .tool(
            "check",
            "Check the project code for errors (runs tsc and ty checks in parallel)",
            check_tool,
        )
        .tool(
            "databricks_apps_logs",
            "Fetch Databricks Apps logs from an already deployed app using the Databricks CLI",
            databricks_apps_logs_tool,
        )
        .tool(
            "search_registry_components",
            "Search shadcn registry components using semantic search. Supports filtering by category, type, and registry.",
            search_registry_components_tool,
        )
        .tool(
            "add_component",
            "Add a component to the project. Component ID can be 'component-name' (from default registry) or '@registry-name/component-name'.",
            add_component_tool,
        )
        .tool(
            "docs",
            "Search Databricks SDK documentation for relevant code examples and API references",
            docs_tool,
        )
        .tool(
            "get_route_info",
            "Get code example for using a specific API route",
            get_route_info_tool,
        )
}

// --- Resources ---

async fn apx_info_resource(_ctx: Arc<AppContext>) -> Result<String, String> {
    Ok(APX_INFO_CONTENT.to_string())
}

#[derive(Serialize)]
struct RouteInfo {
    id: String,
    method: String,
    path: String,
    description: String,
}

async fn routes_resource(ctx: Arc<AppContext>) -> Result<String, String> {
    let metadata = read_project_metadata(&ctx.app_dir)?;
    let (openapi_content, _) =
        generate_openapi_spec(&ctx.app_dir, &metadata.app_entrypoint, &metadata.app_slug)?;

    let openapi: Value = serde_json::from_str(&openapi_content)
        .map_err(|e| format!("Failed to parse OpenAPI schema: {e}"))?;

    let routes = parse_openapi_operations(&openapi)?;

    serde_json::to_string_pretty(&routes).map_err(|e| format!("Failed to serialize routes: {e}"))
}

fn parse_openapi_operations(openapi: &Value) -> Result<Vec<RouteInfo>, String> {
    let mut routes = Vec::new();

    let paths = openapi
        .get("paths")
        .and_then(|p| p.as_object())
        .ok_or_else(|| "OpenAPI schema missing 'paths' object".to_string())?;

    for (path, path_item) in paths {
        let methods_obj = path_item
            .as_object()
            .ok_or_else(|| format!("Path '{path}' is not an object"))?;

        for (method, operation) in methods_obj {
            // Skip non-HTTP method keys like "parameters", "summary", etc.
            let method_upper = method.to_uppercase();
            if !matches!(
                method_upper.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                continue;
            }

            let operation_obj = operation
                .as_object()
                .ok_or_else(|| format!("Operation '{method}' at path '{path}' is not an object"))?;

            let operation_id = operation_obj
                .get("operationId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let description = operation_obj
                .get("summary")
                .or_else(|| operation_obj.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            routes.push(RouteInfo {
                id: operation_id,
                method: method_upper,
                path: path.clone(),
                description,
            });
        }
    }

    Ok(routes)
}

// --- Tools ---

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct LogsArgs {
    #[serde(default = "default_logs_duration")]
    pub duration: String,
}

fn default_logs_duration() -> String {
    crate::cli::dev::logs::DEFAULT_LOG_DURATION.to_string()
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RefreshOpenapiArgs {}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DatabricksAppsLogsArgs {
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default = "default_tail_lines")]
    pub tail_lines: i32,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub source: Option<Vec<String>>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_output")]
    pub output: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: f64,
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: i32,
}

fn default_tail_lines() -> i32 {
    200
}

fn default_output() -> String {
    "text".to_string()
}

fn default_timeout_seconds() -> f64 {
    60.0
}

fn default_max_output_chars() -> i32 {
    20000
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchRegistryComponentsArgs {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    // Note: The following fields are defined for JSON schema but not yet implemented
    #[serde(default)]
    #[allow(dead_code)]
    pub categories: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    pub item_types: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    pub registries: Option<Vec<String>>,
}

fn default_search_limit() -> usize {
    10
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AddComponentArgs {
    /// Component ID: either "component-name" (from default registry) or "@registry-name/component-name"
    pub component_id: String,
    /// Force overwrite existing files
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct DocsArgs {
    /// Documentation source (currently only "databricks-sdk-python" is supported)
    pub source: SDKSource,
    /// Search query (e.g., "create cluster", "list jobs", "databricks connect")
    pub query: String,
    /// Maximum number of results to return
    #[serde(default = "default_docs_limit")]
    pub num_results: usize,
}

fn default_docs_limit() -> usize {
    5
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct GetRouteInfoArgs {
    /// Operation ID from the OpenAPI schema (e.g., "listItems", "createItem")
    pub operation_id: String,
}

async fn start_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::start::start_dev_server;
    use crate::dev::common::CLIENT_HOST;

    match start_dev_server(&ctx.app_dir).await {
        Ok(port) => {
            ToolResult::success(format!("Dev server started at http://{CLIENT_HOST}:{port}"))
        }
        Err(e) => ToolResult::error(e),
    }
}

async fn stop_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::stop::stop_dev_server;

    match stop_dev_server(&ctx.app_dir).await {
        Ok(true) => ToolResult::success("Dev server stopped".to_string()),
        Ok(false) => ToolResult::success("No dev server running".to_string()),
        Err(e) => ToolResult::error(e),
    }
}

async fn restart_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::restart::restart_dev_server;

    match restart_dev_server(&ctx.app_dir).await {
        Ok(port) => ToolResult::success(format!("Dev server restarted at http://localhost:{port}")),
        Err(e) => ToolResult::error(e),
    }
}

async fn logs_tool(ctx: Arc<AppContext>, args: LogsArgs) -> ToolResult {
    use crate::cli::dev::logs::fetch_logs;

    match fetch_logs(&ctx.app_dir, &args.duration).await {
        Ok(logs) => ToolResult::success(logs),
        Err(e) => ToolResult::error(e),
    }
}

async fn refresh_openapi_tool(ctx: Arc<AppContext>, _args: RefreshOpenapiArgs) -> ToolResult {
    use crate::generate_openapi;

    match generate_openapi(&ctx.app_dir) {
        Ok(()) => ToolResult::success("OpenAPI regenerated".to_string()),
        Err(e) => ToolResult::error(e),
    }
}

async fn check_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::check::{CheckArgs, run_inner};

    match run_inner(CheckArgs {
        app_path: Some(ctx.app_dir.clone()),
    })
    .await
    {
        Ok(()) => ToolResult::success("All checks passed".to_string()),
        Err(e) => ToolResult::error(e),
    }
}

async fn databricks_apps_logs_tool(
    ctx: Arc<AppContext>,
    args: DatabricksAppsLogsArgs,
) -> ToolResult {
    let cwd = &ctx.app_dir;
    let mut resolved_from_yml = false;

    // Load env vars from .env if present
    let dotenv_path = cwd.join(".env");
    let dotenv_vars: HashMap<String, String> = if dotenv_path.exists() {
        DotenvFile::read(&dotenv_path)
            .map(|dotenv| dotenv.get_vars())
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    // Resolve app_name if not provided
    let app_name = match args.app_name.as_ref() {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => match resolve_app_name_from_databricks_yml(cwd) {
            Ok(name) => {
                resolved_from_yml = true;
                name
            }
            Err(e) => {
                return ToolResult::error(format!("Failed to auto-detect app name: {e}"));
            }
        },
    };

    // Build command and track arguments for response
    let mut cmd_args = vec!["apps".to_string(), "logs".to_string(), app_name.clone()];
    let mut cmd = Command::new("databricks");
    cmd.args(&cmd_args)
        .arg("--tail-lines")
        .arg(args.tail_lines.to_string())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd_args.push("--tail-lines".to_string());
    cmd_args.push(args.tail_lines.to_string());

    let mut push_flag_value = |flag: &str, value: Option<&str>| {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            cmd.arg(flag).arg(value);
            cmd_args.push(flag.to_string());
            cmd_args.push(value.to_string());
        }
    };

    push_flag_value("--search", args.search.as_deref());
    push_flag_value("-p", args.profile.as_deref());
    push_flag_value("-t", args.target.as_deref());

    if let Some(sources) = &args.source {
        for source in sources {
            cmd.arg("--source").arg(source);
            cmd_args.push("--source".to_string());
            cmd_args.push(source.clone());
        }
    }

    cmd.arg("-o").arg(&args.output);
    cmd_args.push("-o".to_string());
    cmd_args.push(args.output.clone());

    if !dotenv_vars.is_empty() {
        cmd.envs(&dotenv_vars);
    }

    let mut full_command = vec!["databricks".to_string()];
    full_command.extend(cmd_args.clone());
    let cmd_str = full_command.join(" ");

    // Run command with timeout
    let start = Instant::now();
    let result =
        tokio::time::timeout(Duration::from_secs_f64(args.timeout_seconds), cmd.output()).await;

    let (returncode, stdout, stderr, duration_ms) = match result {
        Ok(Ok(output)) => {
            let duration_ms = start.elapsed().as_millis() as i64;
            let returncode = output.status.code().unwrap_or(0);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            (returncode, stdout, stderr, duration_ms)
        }
        Ok(Err(e)) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return ToolResult::error(
                    "Databricks CLI executable not found (`databricks`). \
                    Please install Databricks CLI v0.280.0 or higher and ensure it's on PATH."
                        .to_string(),
                );
            }
            return ToolResult::error(format!("Failed to execute command: {e}"));
        }
        Err(_) => {
            return ToolResult::error(format!(
                "Timed out after {}s running: {}",
                args.timeout_seconds, cmd_str
            ));
        }
    };

    let stdout_t = truncate(&stdout, args.max_output_chars);
    let stderr_t = truncate(&stderr, args.max_output_chars);

    if returncode != 0 {
        let combined = format!("{stderr}\n{stdout}").to_lowercase();
        // Check for unsupported subcommand error
        if combined.contains("unknown command \"logs\"")
            || combined.contains("unknown command logs")
            || combined.contains("unknown subcommand")
            || combined.contains("no such command")
        {
            return ToolResult::error(format!(
                "Databricks CLI does not support `databricks apps logs` in this version. \
                Please upgrade Databricks CLI to v0.280.0 or higher.\n\n\
                Command: {cmd_str}\n\
                Exit code: {returncode}\n\
                stderr:\n{stderr_t}\n\
                stdout:\n{stdout_t}"
            ));
        }

        // Forward any other CLI error
        return ToolResult::error(format!(
            "`databricks apps logs` failed.\n\n\
            Command: {cmd_str}\n\
            Exit code: {returncode}\n\
            stderr:\n{stderr_t}\n\
            stdout:\n{stdout_t}"
        ));
    }

    // Build success response
    #[derive(Serialize)]
    struct DatabricksAppsLogsResponse {
        app_name: String,
        resolved_from_databricks_yml: bool,
        command: Vec<String>,
        cwd: String,
        returncode: i32,
        stdout: String,
        stderr: String,
        duration_ms: i64,
    }

    let response = DatabricksAppsLogsResponse {
        app_name,
        resolved_from_databricks_yml: resolved_from_yml,
        command: full_command,
        cwd: cwd.to_string_lossy().to_string(),
        returncode,
        stdout: stdout_t,
        stderr: stderr_t,
        duration_ms,
    };

    match serde_json::to_string_pretty(&response) {
        Ok(json) => ToolResult::success(json),
        Err(e) => ToolResult::error(format!("Failed to serialize response: {e}")),
    }
}

// Helper functions

fn truncate(s: &str, max_chars: i32) -> String {
    if max_chars <= 0 {
        return String::new();
    }
    let max_chars = max_chars as usize;
    if s.len() <= max_chars {
        return s.to_string();
    }
    let head_len = max_chars.saturating_sub(50);
    let tail_len = if max_chars >= 100 { 40 } else { 0 };
    let head = &s[..head_len];
    let tail = if tail_len > 0 {
        &s[s.len().saturating_sub(tail_len)..]
    } else {
        ""
    };
    let truncated = s.len() - head_len - tail_len;
    format!("{head}\n\n...[truncated {truncated} chars]...\n\n{tail}")
}

fn resolve_app_name_from_databricks_yml(project_dir: &Path) -> Result<String, String> {
    let yml_path = project_dir.join("databricks.yml");
    if !yml_path.exists() {
        return Err(format!(
            "Could not auto-detect app name because databricks.yml was not found at {}. \
            Please pass app_name explicitly.",
            yml_path.display()
        ));
    }

    let contents = std::fs::read_to_string(&yml_path)
        .map_err(|e| format!("Failed to read databricks.yml: {e}"))?;

    let data: Value = serde_yaml::from_str(&contents)
        .map_err(|e| format!("Failed to parse databricks.yml: {e}"))?;

    let resources = data
        .get("resources")
        .ok_or_else(|| "databricks.yml 'resources' must be a mapping/object".to_string())?;

    let apps = resources
        .get("apps")
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())?;

    let apps_obj = apps
        .as_object()
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())?;

    let mut app_names = HashSet::new();
    for app_def in apps_obj.values() {
        if let Some(app_obj) = app_def.as_object() {
            if let Some(name_val) = app_obj.get("name") {
                if let Some(name_str) = name_val.as_str() {
                    let name = name_str.trim();
                    if !name.is_empty() {
                        app_names.insert(name.to_string());
                    }
                }
            }
        }
    }

    let mut app_names_vec: Vec<String> = app_names.into_iter().collect();
    app_names_vec.sort();

    match app_names_vec.len() {
        1 => Ok(app_names_vec[0].clone()),
        0 => Err(
            "Could not auto-detect app name because no apps were found in databricks.yml under \
            resources.apps.*.name. Please pass app_name explicitly."
                .to_string(),
        ),
        _ => Err(format!(
            "Could not auto-detect app name because multiple apps were found in databricks.yml \
            ({}). Please pass app_name explicitly.",
            app_names_vec.join(", ")
        )),
    }
}

async fn search_registry_components_tool(
    ctx: Arc<AppContext>,
    args: SearchRegistryComponentsArgs,
) -> ToolResult {
    use crate::cli::components::UiConfig;
    use crate::common::read_project_metadata;

    // Wait for component index to be ready (15 second timeout)
    if let Err(e) = wait_for_index_ready(
        &ctx.index_state.component_ready,
        &ctx.index_state.component_indexed,
        "Component",
    )
    .await
    {
        return ToolResult::error(e);
    }

    // Check if registry indexes need refresh
    if let Ok(metadata) = read_project_metadata(&ctx.app_dir) {
        let cfg = UiConfig::from_metadata(&metadata, &ctx.app_dir);
        if needs_registry_refresh(&cfg.registries) {
            tracing::info!("Registry indexes stale, refreshing...");
            if let Ok(true) = sync_registry_indexes(&ctx.app_dir, false).await {
                // Rebuild search index
                if let Err(e) = rebuild_search_index().await {
                    tracing::warn!("Failed to rebuild search index after refresh: {}", e);
                }
            }
        }
    }

    // Get or create index
    let db_path = match ComponentIndex::default_path() {
        Ok(path) => path,
        Err(e) => return ToolResult::error(format!("Failed to get database path: {e}")),
    };

    let index = match ComponentIndex::new(db_path) {
        Ok(idx) => idx,
        Err(e) => return ToolResult::error(format!("Failed to initialize index: {e}")),
    };

    let table_name = ComponentIndex::table_name("components");

    // Search - index should be ready at this point
    let search_results = match index.search(&table_name, &args.query, args.limit).await {
        Ok(results) => results,
        Err(e) => return ToolResult::error(format!("Search failed: {e}")),
    };

    #[derive(serde::Serialize)]
    struct SearchResponse {
        query: String,
        results: Vec<SearchResultItem>,
    }

    #[derive(serde::Serialize)]
    struct SearchResultItem {
        id: String,
        name: String,
        registry: String,
        score: f32,
    }

    let response = SearchResponse {
        query: args.query,
        results: search_results
            .into_iter()
            .map(|r| SearchResultItem {
                id: r.id,
                name: r.name,
                registry: r.registry,
                score: r.score,
            })
            .collect(),
    };

    match serde_json::to_string_pretty(&response) {
        Ok(json) => ToolResult::success(json),
        Err(e) => ToolResult::error(format!("Failed to serialize results: {e}")),
    }
}

async fn add_component_tool(ctx: Arc<AppContext>, args: AddComponentArgs) -> ToolResult {
    use crate::cli::components::add::{ComponentsAddArgs, run_inner};

    // Parse component ID to extract registry and component name
    let (registry, component) = if args.component_id.starts_with('@') {
        if let Some((prefix, name)) = args.component_id.split_once('/') {
            (Some(prefix.to_string()), name.to_string())
        } else {
            return ToolResult::error(format!(
                "Invalid component ID format: {}. Expected '@registry-name/component-name'",
                args.component_id
            ));
        }
    } else {
        (None, args.component_id.clone())
    };

    // Create ComponentsAddArgs
    let add_args = ComponentsAddArgs {
        component,
        registry,
        force: args.force,
        dry_run: false,
        app_path: Some(ctx.app_dir.clone()),
    };

    // Call run_inner (this will fetch and cache the component)
    match run_inner(add_args).await {
        Ok(()) => {
            // Component added and cached - index will be updated on next search
            // Note: We don't rebuild the entire index here as it's expensive.
            // The component is already cached, and the next cache population
            // cycle will include it in the index.
            tracing::info!("Component {} added successfully", args.component_id);
            ToolResult::success(format!(
                "Successfully added component: {}",
                args.component_id
            ))
        }
        Err(e) => ToolResult::error(format!("Failed to add component: {e}")),
    }
}

async fn docs_tool(ctx: Arc<AppContext>, args: DocsArgs) -> ToolResult {
    // Wait for SDK index to be ready (15 second timeout)
    if let Err(e) = wait_for_index_ready(
        &ctx.index_state.sdk_ready,
        &ctx.index_state.sdk_indexed,
        "SDK documentation",
    )
    .await
    {
        return ToolResult::error(e);
    }

    // Get the SDK doc index
    let index_guard = ctx.sdk_doc_index.lock().await;

    let index = match index_guard.as_ref() {
        Some(idx) => idx,
        None => {
            return ToolResult::error(
                "SDK documentation is not available. The Databricks SDK may not be installed or the index failed to bootstrap.".to_string()
            );
        }
    };

    // Search
    match index
        .search(&args.source, &args.query, args.num_results)
        .await
    {
        Ok(results) => {
            #[derive(Serialize)]
            struct DocsResponse {
                source: String,
                query: String,
                results: Vec<DocsResult>,
            }

            #[derive(Serialize)]
            struct DocsResult {
                text: String,
                source_file: String,
                score: f32,
            }

            let response = DocsResponse {
                source: match args.source {
                    SDKSource::DatabricksSdkPython => "databricks-sdk-python".to_string(),
                },
                query: args.query,
                results: results
                    .into_iter()
                    .map(|r| DocsResult {
                        text: r.text,
                        source_file: r.source_file,
                        score: r.score,
                    })
                    .collect(),
            };

            match serde_json::to_string_pretty(&response) {
                Ok(json) => ToolResult::success(json),
                Err(e) => ToolResult::error(format!("Failed to serialize results: {e}")),
            }
        }
        Err(e) => ToolResult::error(e),
    }
}

async fn get_route_info_tool(ctx: Arc<AppContext>, args: GetRouteInfoArgs) -> ToolResult {
    let metadata = match read_project_metadata(&ctx.app_dir) {
        Ok(m) => m,
        Err(e) => return ToolResult::error(e),
    };

    let openapi_content =
        match generate_openapi_spec(&ctx.app_dir, &metadata.app_entrypoint, &metadata.app_slug) {
            Ok((content, _)) => content,
            Err(e) => return ToolResult::error(format!("Failed to generate OpenAPI spec: {e}")),
        };

    let openapi: Value = match serde_json::from_str(&openapi_content) {
        Ok(spec) => spec,
        Err(e) => return ToolResult::error(format!("Failed to parse OpenAPI schema: {e}")),
    };

    // Find the operation by operationId
    let paths = match openapi.get("paths").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return ToolResult::error("OpenAPI schema missing 'paths' object".to_string()),
    };

    let mut found_method = None;
    for (_, path_item) in paths {
        if let Some(methods_obj) = path_item.as_object() {
            for (method, operation) in methods_obj {
                if let Some(operation_obj) = operation.as_object() {
                    if let Some(op_id) = operation_obj.get("operationId").and_then(|v| v.as_str()) {
                        if op_id == args.operation_id {
                            found_method = Some(method.to_uppercase());
                            break;
                        }
                    }
                }
            }
            if found_method.is_some() {
                break;
            }
        }
    }

    let method = match found_method {
        Some(m) => m,
        None => {
            return ToolResult::error(format!(
                "Operation ID '{}' not found in OpenAPI schema",
                args.operation_id
            ));
        }
    };

    // Generate the appropriate code example based on the HTTP method
    let example = if method == "GET" {
        generate_query_example(&args.operation_id)
    } else {
        generate_mutation_example(&args.operation_id)
    };

    #[derive(Serialize)]
    struct RouteInfoResponse {
        operation_id: String,
        method: String,
        example: String,
    }

    let response = RouteInfoResponse {
        operation_id: args.operation_id,
        method,
        example,
    };

    match serde_json::to_string_pretty(&response) {
        Ok(json) => ToolResult::success(json),
        Err(e) => ToolResult::error(format!("Failed to serialize response: {e}")),
    }
}

fn generate_query_example(operation_id: &str) -> String {
    // Convert operationId to PascalCase for the hook name
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");
    let suspense_hook_name = format!("{hook_name}Suspense");
    let result_type = format!("{capitalized}QueryResult");
    let error_type = format!("{capitalized}QueryError");

    format!(
        r#"// Standard query hook
import {{ {hook_name} }} from "@/lib/api";
import selector from "@/lib/selector";

const Component = () => {{
  const {{ data, isLoading, error }} = {hook_name}(selector());
  
  if (isLoading) return <div>Loading...</div>;
  if (error) return <div>Error: {{error.message}}</div>;
  
  return <div>{{/* render data */}}</div>;
}};

// Suspense query hook (use with React Suspense boundary)
import {{ {suspense_hook_name} }} from "@/lib/api";
import selector from "@/lib/selector";

const SuspenseComponent = () => {{
  // No loading/error states needed - handled by Suspense boundary
  const {{ data }} = {suspense_hook_name}(selector());
  return <div>{{/* render data */}}</div>;
}};

// Usage with Suspense boundary:
// <Suspense fallback={{<Loading />}}>
//   <SuspenseComponent />
// </Suspense>

// Available types for this query:
// import type {{ {result_type}, {error_type} }} from "@/lib/api";"#
    )
}

fn generate_mutation_example(operation_id: &str) -> String {
    // Convert operationId to PascalCase for the hook name
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");
    let body_type = format!("{capitalized}MutationBody");
    let result_type = format!("{capitalized}MutationResult");
    let error_type = format!("{capitalized}MutationError");

    format!(
        r#"import {{ {hook_name} }} from "@/lib/api";

const Component = () => {{
  const {{ mutate, isPending }} = {hook_name}();
  
  const handleSubmit = () => {{
    mutate({{ data: {{ /* request body */ }} }});
  }};
  
  return <button onClick={{handleSubmit}}>Submit</button>;
}};

// Available types for this mutation:
// import type {{ {body_type}, {result_type}, {error_type} }} from "@/lib/api";"#
    )
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

const APX_INFO_CONTENT: &str = r#"

this project uses apx toolkit to build a Databricks app. 
apx bundles together a set of tools and libraries to help you with the complete app development lifecycle: develop, build and deploy.

## Technology Stack

- **Backend**: Python + FastAPI + Pydantic
- **Frontend**: React + TypeScript + shadcn/ui
- **Build Tools**: uv (Python), bun (JavaScript/TypeScript)

"#;
