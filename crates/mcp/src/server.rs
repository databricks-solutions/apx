use crate::context::{AppContext, SdkIndexParams};
use crate::indexing::init_all_indexes;
use crate::info_content::APX_INFO_CONTENT;
use crate::tools::AppPathArgs;
use crate::tools::databricks::DatabricksAppsLogsArgs;
use crate::tools::devserver::LogsToolArgs;
use crate::tools::docs::DocsArgs;
use crate::tools::project::GetRouteInfoArgs;
use crate::tools::registry::{
    AddComponentArgs, ListRegistryComponentsArgs, SearchRegistryComponentsArgs,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{RoleServer, ServerHandler, service::RequestContext, tool, tool_handler, tool_router};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApxServer {
    pub ctx: Arc<AppContext>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for ApxServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApxServer").finish_non_exhaustive()
    }
}

// All tool definitions in a single #[tool_router] block.
// Heavy logic is delegated to handler methods in tools/*.rs modules.
#[tool_router]
impl ApxServer {
    pub fn new(ctx: AppContext, sdk_params: Option<SdkIndexParams>) -> Self {
        // Initialize all indexes in background
        let shutdown_rx = ctx.shutdown_tx.subscribe();
        init_all_indexes(&ctx, shutdown_rx, sdk_params);

        Self {
            ctx: Arc::new(ctx),
            tool_router: Self::tool_router(),
        }
    }

    // --- Dev server tools ---

    #[tool(
        name = "start",
        description = "Start the development server and return its URL. Call before testing UI or API changes.",
        annotations(destructive_hint = true, read_only_hint = false)
    )]
    async fn start(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_start(args).await
    }

    #[tool(
        name = "stop",
        description = "Stop the development server.",
        annotations(
            destructive_hint = true,
            read_only_hint = false,
            idempotent_hint = true
        )
    )]
    async fn stop(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_stop(args).await
    }

    #[tool(
        name = "restart",
        description = "Restart the development server (preserves port). Use after backend code changes.",
        annotations(destructive_hint = true, read_only_hint = false)
    )]
    async fn restart(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_restart(args).await
    }

    #[tool(
        name = "logs",
        description = "Fetch recent dev server logs. Use to diagnose runtime errors or startup issues.",
        annotations(read_only_hint = true)
    )]
    async fn logs(
        &self,
        Parameters(args): Parameters<LogsToolArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_logs(args).await
    }

    // --- Project tools ---

    #[tool(
        name = "check",
        description = "Run TypeScript and Python type checks in parallel. Returns categorized errors. Call after making changes to verify correctness.",
        annotations(read_only_hint = true)
    )]
    async fn check(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_check(args).await
    }

    #[tool(
        name = "refresh_openapi",
        description = "Regenerate the OpenAPI schema and TypeScript API client from backend routes. Run after adding or modifying backend routes.",
        annotations(
            destructive_hint = true,
            read_only_hint = false,
            idempotent_hint = true
        )
    )]
    async fn refresh_openapi(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_refresh_openapi(args).await
    }

    #[tool(
        name = "get_route_info",
        description = "Get a complete frontend code example for a specific API route, including Suspense/ErrorBoundary scaffold and correct hook usage with parameters. Call this before writing any frontend code that uses an API route. Pass the operation_id from the routes tool.",
        annotations(read_only_hint = true)
    )]
    async fn get_route_info(
        &self,
        Parameters(args): Parameters<GetRouteInfoArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_get_route_info(args).await
    }

    #[tool(
        name = "routes",
        description = "List all API routes with their parameters, request/response schemas, and generated hook names. Call this first to understand the project's API surface before reading source files.",
        annotations(read_only_hint = true)
    )]
    async fn routes(
        &self,
        Parameters(args): Parameters<AppPathArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_routes(args).await
    }

    // --- Databricks tools ---

    #[tool(
        name = "databricks_apps_logs",
        description = "Fetch logs from a deployed Databricks App using the Databricks CLI. Use for debugging deployed (not local dev) issues.",
        annotations(read_only_hint = true)
    )]
    async fn databricks_apps_logs(
        &self,
        Parameters(args): Parameters<DatabricksAppsLogsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_databricks_apps_logs(args).await
    }

    // --- Registry tools ---

    #[tool(
        name = "search_registry_components",
        description = "Semantic search for UI components across configured registries (shadcn, etc). Returns component IDs usable with add_component.",
        annotations(read_only_hint = true)
    )]
    async fn search_registry_components(
        &self,
        Parameters(args): Parameters<SearchRegistryComponentsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_search_registry_components(args).await
    }

    #[tool(
        name = "add_component",
        description = "Install a UI component into the project. Accepts 'component-name' (default registry) or '@registry-name/component-name'.",
        annotations(destructive_hint = true, read_only_hint = false)
    )]
    async fn add_component(
        &self,
        Parameters(args): Parameters<AddComponentArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_add_component(args).await
    }

    #[tool(
        name = "list_registry_components",
        description = "List all available components in a registry. Defaults to shadcn registry if none specified.",
        annotations(read_only_hint = true)
    )]
    async fn list_registry_components(
        &self,
        Parameters(args): Parameters<ListRegistryComponentsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_list_registry_components(args).await
    }

    // --- Docs tools ---

    #[tool(
        name = "docs",
        description = "Search Databricks SDK documentation for Python code examples and API references. Always call this before writing any Databricks SDK (ws.*) call to verify the correct method signature.",
        annotations(read_only_hint = true)
    )]
    async fn docs(
        &self,
        Parameters(args): Parameters<DocsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.handle_docs(args).await
    }
}

#[tool_handler]
impl ServerHandler for ApxServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "apx".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("apx - the toolkit for building Databricks Apps".into()),
                description: None,
                icons: None,
                website_url: Some(
                    "https://databricks-solutions.github.io/apx/docs/reference/mcp".into(),
                ),
            },
            instructions: Some(APX_INFO_CONTENT.to_string()),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        Ok(ListResourcesResult {
            resources: crate::resources::list_resources(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, rmcp::ErrorData> {
        Ok(ListResourceTemplatesResult {
            resource_templates: crate::resources::list_resource_templates(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
        let uri = request.uri.as_str();

        if let Some(app_path) = uri.strip_prefix("apx://project/") {
            return crate::resources::read_project_resource(app_path)
                .await
                .map_err(|e| {
                    rmcp::ErrorData::resource_not_found(e, Some(serde_json::json!({ "uri": uri })))
                });
        }

        crate::resources::read_resource(uri).map_err(|e| {
            rmcp::ErrorData::resource_not_found(e, Some(serde_json::json!({ "uri": uri })))
        })
    }
}

pub async fn run_server(ctx: AppContext, sdk_params: Option<SdkIndexParams>) -> Result<(), String> {
    use rmcp::ServiceExt;

    let shutdown_tx = ctx.shutdown_tx.clone();
    let server = ApxServer::new(ctx, sdk_params);
    let transport = rmcp::transport::io::stdio();
    let service = server
        .serve(transport)
        .await
        .map_err(|e| format!("MCP server initialization error: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("MCP server error: {e}"))?;
    let _ = shutdown_tx.send(());
    Ok(())
}
