use crate::indexing::{rebuild_search_index, wait_for_index_ready};
use crate::server::ApxServer;
use crate::tools::ToolResultExt;
use crate::validation::validate_app_path;
use apx_core::components::{needs_registry_refresh, sync_registry_indexes};
use apx_core::search::ComponentIndex;
use rmcp::model::*;
use rmcp::schemars;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchRegistryComponentsArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Search query
    pub query: String,
    /// Maximum number of results (default: 10)
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    10
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddComponentArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Component ID: "component-name" or "@registry-name/component-name"
    pub component_id: String,
    /// Force overwrite existing files
    #[serde(default)]
    pub force: bool,
}

impl ApxServer {
    pub async fn handle_search_registry_components(
        &self,
        args: SearchRegistryComponentsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        let ctx = &self.ctx;

        // Wait for component index to be ready (15 second timeout)
        if let Err(e) = wait_for_index_ready(
            &ctx.index_state.component_ready,
            &ctx.index_state.component_indexed,
            "Component",
        )
        .await
        {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }

        // Check if registry indexes need refresh
        if let Ok(metadata) = apx_core::common::read_project_metadata(&path) {
            let cfg = apx_core::components::UiConfig::from_metadata(&metadata, &path);
            if needs_registry_refresh(&cfg.registries) {
                tracing::info!("Registry indexes stale, refreshing...");
                if let Ok(true) = sync_registry_indexes(&path, false).await {
                    let rebuild_result = tokio::task::spawn_blocking(rebuild_search_index).await;
                    if let Ok(Err(e)) = rebuild_result {
                        tracing::warn!("Failed to rebuild search index after refresh: {}", e);
                    }
                }
            }
        }

        // Search in spawn_blocking (sync SQLite operations)
        let search_query = args.query.clone();
        let limit = args.limit;
        let search_results = match tokio::task::spawn_blocking(move || {
            let index = ComponentIndex::new()?;
            index.search(&search_query, limit)
        })
        .await
        {
            Ok(Ok(results)) => results,
            Ok(Err(e)) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Search failed: {e}"
                ))]));
            }
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Search task panicked: {e}"
                ))]));
            }
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

        Ok(CallToolResult::from_serializable(&response))
    }

    pub async fn handle_add_component(
        &self,
        args: AddComponentArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::components::add::{ComponentInput, add_components};

        let input = if args.component_id.starts_with('@') {
            if let Some((prefix, name)) = args.component_id.split_once('/') {
                ComponentInput::with_registry(name, prefix)
            } else {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Invalid component ID format: {}. Expected '@registry-name/component-name'",
                    args.component_id
                ))]));
            }
        } else {
            ComponentInput::new(args.component_id.clone())
        };

        match add_components(&path, &[input], args.force).await {
            Ok(_result) => {
                tracing::info!("Component {} added successfully", args.component_id);
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Successfully added component: {}",
                    args.component_id
                ))]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to add component: {e}"
            ))])),
        }
    }
}
