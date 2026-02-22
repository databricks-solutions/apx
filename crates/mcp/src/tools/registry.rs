use std::collections::HashSet;

use crate::indexing::{rebuild_search_index, wait_for_index_ready};
use crate::server::ApxServer;
use crate::tools::{ToolError, ToolResultExt};
use crate::validation::validated_app_path;
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
        let path = validated_app_path(&args.app_path)?;

        let ctx = &self.ctx;

        // Wait for component index to be ready (15 second timeout)
        if let Err(e) = wait_for_index_ready(
            &ctx.index_state.component_ready,
            &ctx.index_state.component_indexed,
            "Component",
        )
        .await
        {
            return ToolError::IndexNotReady(e).into_result();
        }

        // Check if registry indexes need refresh and collect configured registry names
        let configured_registries = self.refresh_registries_if_stale(&path).await;

        // Search using async DB layer
        let pool = self.ctx.dev_db.pool().clone();
        let index = match ComponentIndex::new(pool) {
            Ok(idx) => idx,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to create index: {e}"))
                    .into_result();
            }
        };
        let search_results = match index
            .search(&args.query, args.limit, configured_registries.as_ref())
            .await
        {
            Ok(results) => results,
            Err(e) => {
                return ToolError::OperationFailed(format!("Search failed: {e}")).into_result();
            }
        };

        tool_response! {
            struct SearchResponse {
                query: String,
                configured_registries: Vec<String>,
                results: Vec<SearchResultItem>,
            }
        }

        #[derive(Debug, serde::Serialize)]
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
        let path = validated_app_path(&args.app_path)?;

        use apx_core::components::add::{ComponentInput, add_components};

        let input = if args.component_id.starts_with('@') {
            if let Some((prefix, name)) = args.component_id.split_once('/') {
                ComponentInput::with_registry(name, prefix)
            } else {
                return ToolError::InvalidInput(format!(
                    "Invalid component ID format: {}. Expected '@registry-name/component-name'",
                    args.component_id
                ))
                .into_result();
            }
        } else {
            ComponentInput::new(args.component_id.clone())
        };

        match add_components(&path, &[input], args.force).await {
            Ok(result) => {
                tracing::info!("Component {} added successfully", args.component_id);

                tool_response! {
                    struct AddResponse {
                        component_id: String,
                        written_files: Vec<String>,
                        unchanged_files: Vec<String>,
                        dependencies_installed: Vec<String>,
                        auto_detected_deps: Vec<String>,
                        css_updated: Option<String>,
                        warnings: Vec<String>,
                    }
                }

                let response = AddResponse {
                    component_id: args.component_id,
                    written_files: result
                        .written_paths
                        .iter()
                        .map(|p| apx_core::components::utils::format_relative_path(p, &path))
                        .collect(),
                    unchanged_files: result
                        .unchanged_paths
                        .iter()
                        .map(|p| apx_core::components::utils::format_relative_path(p, &path))
                        .collect(),
                    dependencies_installed: result.dependencies_installed,
                    auto_detected_deps: result.auto_detected_deps,
                    css_updated: result.css_updated_path,
                    warnings: result.warnings,
                };

                Ok(CallToolResult::from_serializable(&response))
            }
            Err(e) => {
                ToolError::OperationFailed(format!("Failed to add component: {e}")).into_result()
            }
        }
    }

    pub async fn handle_list_registry_components(
        &self,
        args: ListRegistryComponentsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validated_app_path(&args.app_path)?;

        // Check if registry indexes need refresh
        let _ = self.refresh_registries_if_stale(&path).await;

        let all_indexes = match get_all_registry_indexes() {
            Ok(indexes) => indexes,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to load registry indexes: {e}"))
                    .into_result();
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
                return ToolError::OperationFailed(format!(
                    "Registry '{}' not found. Available registries: {:?}",
                    registry_key, available
                ))
                .into_result();
            }
        };

        tool_response! {
            struct ListResponse {
                registry: String,
                total: usize,
                items: Vec<ListItem>,
            }
        }

        #[derive(Debug, serde::Serialize)]
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

    /// Read project metadata + UiConfig, refresh stale registry indexes,
    /// and return the set of configured registry names (if available).
    async fn refresh_registries_if_stale(&self, path: &std::path::Path) -> Option<HashSet<String>> {
        let metadata = apx_core::common::read_project_metadata(path).ok()?;
        let cfg = apx_core::components::UiConfig::from_metadata(&metadata, path).ok()?;
        let registries: HashSet<String> = cfg.registries.keys().cloned().collect();

        if needs_registry_refresh(&cfg.registries) {
            tracing::info!("Registry indexes stale, refreshing...");
            if let Ok(true) = sync_registry_indexes(path, false).await {
                let pool = self.ctx.dev_db.pool().clone();
                if let Err(e) = rebuild_search_index(pool).await {
                    tracing::warn!("Failed to rebuild search index after refresh: {}", e);
                }
            }
        }

        Some(registries)
    }
}
