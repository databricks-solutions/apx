use std::collections::HashSet;

use crate::indexing::{rebuild_search_index, wait_for_index_ready};
use crate::server::ApxServer;
use crate::tools::ToolResultExt;
use crate::validation::validate_app_path;
use apx_core::components::{
    get_all_registry_indexes, needs_registry_refresh, sync_registry_indexes,
};
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListRegistryComponentsArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Registry name (e.g. "@animate-ui"). Omit or leave empty for default shadcn registry.
    pub registry: Option<String>,
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

        // Check if registry indexes need refresh and collect configured registry names
        let mut configured_registries: Option<HashSet<String>> = None;
        if let Ok(metadata) = apx_core::common::read_project_metadata(&path)
            && let Ok(cfg) = apx_core::components::UiConfig::from_metadata(&metadata, &path)
        {
            configured_registries = Some(cfg.registries.keys().cloned().collect());
            if needs_registry_refresh(&cfg.registries) {
                tracing::info!("Registry indexes stale, refreshing...");
                if let Ok(true) = sync_registry_indexes(&path, false).await {
                    let pool = self.ctx.dev_db.pool().clone();
                    if let Err(e) = rebuild_search_index(pool.clone()).await {
                        tracing::warn!("Failed to rebuild search index after refresh: {}", e);
                    }
                }
            }
        }

        // Search using async DB layer
        let pool = self.ctx.dev_db.pool().clone();
        let index = ComponentIndex::new(pool);
        let search_results = match index
            .search(&args.query, args.limit, configured_registries.as_ref())
            .await
        {
            Ok(results) => results,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Search failed: {e}"
                ))]));
            }
        };

        #[derive(serde::Serialize)]
        struct SearchResponse {
            query: String,
            configured_registries: Vec<String>,
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
            configured_registries: configured_registries
                .map(|cr| cr.into_iter().collect::<Vec<_>>())
                .unwrap_or_default(),
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

    pub async fn handle_list_registry_components(
        &self,
        args: ListRegistryComponentsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        // Check if registry indexes need refresh
        if let Ok(metadata) = apx_core::common::read_project_metadata(&path) {
            let cfg = apx_core::components::UiConfig::from_metadata(&metadata, &path)
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
            if needs_registry_refresh(&cfg.registries) {
                tracing::info!("Registry indexes stale, refreshing...");
                if let Ok(true) = sync_registry_indexes(&path, false).await {
                    let pool = self.ctx.dev_db.pool().clone();
                    if let Err(e) = rebuild_search_index(pool.clone()).await {
                        tracing::warn!("Failed to rebuild search index after refresh: {}", e);
                    }
                }
            }
        }

        let all_indexes = match get_all_registry_indexes() {
            Ok(indexes) => indexes,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to load registry indexes: {e}"
                ))]));
            }
        };

        // Determine which registry key to look up
        let registry_key = match &args.registry {
            Some(name) if !name.is_empty() => name.trim_start_matches('@').to_string(),
            _ => "ui".to_string(),
        };

        let items = match all_indexes.get(&registry_key) {
            Some(items) => items,
            None => {
                let available: Vec<&String> = all_indexes.keys().collect();
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Registry '{}' not found. Available registries: {:?}",
                    registry_key, available
                ))]));
            }
        };

        #[derive(serde::Serialize)]
        struct ListResponse {
            registry: String,
            total: usize,
            items: Vec<ListItem>,
        }

        #[derive(serde::Serialize)]
        struct ListItem {
            name: String,
            description: Option<String>,
            dependencies: usize,
            registry_dependencies: usize,
        }

        let response = ListResponse {
            registry: registry_key,
            total: items.len(),
            items: items
                .iter()
                .map(|item| ListItem {
                    name: item.name.clone(),
                    description: item.description.clone(),
                    dependencies: item.dependencies.len(),
                    registry_dependencies: item.registry_dependencies.len(),
                })
                .collect(),
        };

        Ok(CallToolResult::from_serializable(&response))
    }
}
