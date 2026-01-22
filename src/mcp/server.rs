use crate::mcp::core::{McpServer, ToolResult};
use crate::dotenv::DotenvFile;
use crate::databricks_sdk_doc::{SDKDocIndex, SDKSource};
use crate::cli::components::{SharedCacheState, sync_registry_indexes, needs_registry_refresh};
use crate::search::ComponentIndex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;

pub struct AppContext {
    pub app_dir: PathBuf,
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocIndex>>>,
    pub cache_state: SharedCacheState,
}

/// Initialize registry indexes and search index in background
/// This is called when the MCP server starts
pub fn init_cache_and_index(ctx: &AppContext) {
    let app_dir = ctx.app_dir.clone();
    let cache_state = ctx.cache_state.clone();

    tokio::spawn(async move {
        // Mark as running
        {
            let mut guard = cache_state.lock().await;
            guard.is_running = true;
        }

        tracing::info!("Syncing registry indexes on MCP start");

        // Sync registry.json files (prefetches default shadcn items only)
        match sync_registry_indexes(&app_dir, false).await {
            Ok(refreshed) => {
                if refreshed {
                    tracing::info!("Registry indexes refreshed, rebuilding search index");
                    if let Err(e) = rebuild_search_index().await {
                        tracing::warn!("Failed to rebuild search index: {}", e);
                    }
                } else {
                    // Check if search index exists, build if not
                    if let Err(e) = ensure_search_index().await {
                        tracing::warn!("Failed to ensure search index: {}", e);
                    }
                }
            }
            Err(e) => tracing::warn!("Failed to sync registry indexes: {}", e),
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

/// Ensure search index exists, build if not
async fn ensure_search_index() -> Result<(), String> {
    let db_path = ComponentIndex::default_path()?;
    let index = ComponentIndex::new(db_path)?;
    let table_name = ComponentIndex::table_name("components");

    // Check if table exists
    if !index.table_exists_pub(&table_name).await? {
        tracing::info!("Search index not found, building from registry indexes");
        index.build_index_from_registries(&table_name).await?;
    }
    Ok(())
}

pub fn build_server(ctx: AppContext) -> McpServer<AppContext> {
    // Initialize cache and index in background
    init_cache_and_index(&ctx);

    McpServer::new(ctx)
        .resource(
            "apx://info",
            "apx-info",
            "Information about apx toolkit",
            "text/plain",
            apx_info_resource,
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
}

// --- Resources ---

async fn apx_info_resource(_ctx: Arc<AppContext>) -> Result<String, String> {
    Ok(APX_INFO_CONTENT.to_string())
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
pub struct RefreshOpenapiArgs {
    #[serde(default)]
    pub force: bool,
}

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

async fn start_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::start::start_dev_server;
    use crate::dev::common::CLIENT_HOST;

    match start_dev_server(&ctx.app_dir).await {
        Ok(port) => ToolResult::success(format!(
            "Dev server started at http://{}:{}",
            CLIENT_HOST, port
        )),
        Err(e) => ToolResult::error(e),
    }
}

async fn stop_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::stop::stop_dev_server;

    match stop_dev_server(&ctx.app_dir).await {
        Ok(()) => ToolResult::success("Dev server stopped".to_string()),
        Err(e) => ToolResult::error(e),
    }
}

async fn restart_tool(ctx: Arc<AppContext>, _args: EmptyArgs) -> ToolResult {
    use crate::cli::dev::restart::restart_dev_server;
    use crate::dev::common::CLIENT_HOST;

    match restart_dev_server(&ctx.app_dir).await {
        Ok(port) => ToolResult::success(format!(
            "Dev server restarted at http://{}:{}",
            CLIENT_HOST, port
        )),
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

async fn refresh_openapi_tool(ctx: Arc<AppContext>, args: RefreshOpenapiArgs) -> ToolResult {
    use crate::generate_openapi;

    match generate_openapi(&ctx.app_dir, args.force) {
        Ok(true) => ToolResult::success("OpenAPI regenerated".to_string()),
        Ok(false) => ToolResult::success("OpenAPI is up to date".to_string()),
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
        _ => {
            match resolve_app_name_from_databricks_yml(cwd) {
                Ok(name) => {
                    resolved_from_yml = true;
                    name
                }
                Err(e) => {
                    return ToolResult::error(format!("Failed to auto-detect app name: {e}"));
                }
            }
        }
    };

    // Build command and track arguments for response
    let mut cmd_args = vec![
        "apps".to_string(),
        "logs".to_string(),
        app_name.clone(),
    ];
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
    let result = tokio::time::timeout(
        Duration::from_secs_f64(args.timeout_seconds),
        cmd.output(),
    )
    .await;

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
        let combined = format!("{}\n{}", stderr, stdout).to_lowercase();
        // Check for unsupported subcommand error
        if combined.contains("unknown command \"logs\"")
            || combined.contains("unknown command logs")
            || combined.contains("unknown subcommand")
            || combined.contains("no such command")
        {
            return ToolResult::error(format!(
                "Databricks CLI does not support `databricks apps logs` in this version. \
                Please upgrade Databricks CLI to v0.280.0 or higher.\n\n\
                Command: {}\n\
                Exit code: {}\n\
                stderr:\n{}\n\
                stdout:\n{}",
                cmd_str, returncode, stderr_t, stdout_t
            ));
        }

        // Forward any other CLI error
        return ToolResult::error(format!(
            "`databricks apps logs` failed.\n\n\
            Command: {}\n\
            Exit code: {}\n\
            stderr:\n{}\n\
            stdout:\n{}",
            cmd_str, returncode, stderr_t, stdout_t
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
    format!("{}\n\n...[truncated {} chars]...\n\n{}", head, truncated, tail)
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
    use crate::common::read_project_metadata;
    use crate::cli::components::UiConfig;

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
        Err(e) => return ToolResult::error(format!("Failed to get database path: {}", e)),
    };

    let index = match ComponentIndex::new(db_path) {
        Ok(idx) => idx,
        Err(e) => return ToolResult::error(format!("Failed to initialize index: {}", e)),
    };

    let table_name = ComponentIndex::table_name("components");

    // Search, building index if needed
    let search_results = match index.search(&table_name, &args.query, args.limit).await {
        Ok(results) => results,
        Err(e) if e.contains("Index not built") => {
            // Check if indexing is running
            let is_running = {
                let guard = ctx.cache_state.lock().await;
                guard.is_running
            };

            if is_running {
                return ToolResult::error(
                    "Component index is being built in background. Please try again shortly.".to_string()
                );
            }

            // Build index from registry indexes
            tracing::info!("Index not found, building from registry indexes...");
            if let Err(build_err) = index.build_index_from_registries(&table_name).await {
                return ToolResult::error(format!("Failed to build index: {}", build_err));
            }

            match index.search(&table_name, &args.query, args.limit).await {
                Ok(results) => results,
                Err(e) => return ToolResult::error(format!("Search failed after building index: {}", e)),
            }
        }
        Err(e) => return ToolResult::error(format!("Search failed: {}", e)),
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
        Err(e) => ToolResult::error(format!("Failed to serialize results: {}", e)),
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
            ToolResult::success(format!("Successfully added component: {}", args.component_id))
        }
        Err(e) => ToolResult::error(format!("Failed to add component: {}", e)),
    }
}

async fn docs_tool(ctx: Arc<AppContext>, args: DocsArgs) -> ToolResult {
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
    match index.search(&args.source, &args.query, args.num_results).await {
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
                results: results.into_iter().map(|r| DocsResult {
                    text: r.text,
                    source_file: r.source_file,
                    score: r.score,
                }).collect(),
            };

            match serde_json::to_string_pretty(&response) {
                Ok(json) => ToolResult::success(json),
                Err(e) => ToolResult::error(format!("Failed to serialize results: {}", e)),
            }
        }
        Err(e) => ToolResult::error(e),
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
