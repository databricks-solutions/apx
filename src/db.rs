use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::cli::components::{RegistryItem, RegistryCatalogEntry, ResolvedRequest};

// Global singleton for the registry database
static REGISTRY_DB: OnceLock<RegistryDb> = OnceLock::new();

// Configuration for the database
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DbConfig {
    pub db_path: String,
    pub cache_ttl_hours: i64,
}

// Search result structure
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub item_type: String,
    pub categories: Vec<String>,
    pub registry_name: Option<String>,
    pub similarity_score: f32,
}

/// Get or initialize the global registry database instance
///
/// This function returns a reference to the singleton RegistryDb instance.
/// The database is initialized on first access with the default configuration
/// stored in ~/.apx/db. If initialization fails, it will be retried on next access.
pub async fn get_db() -> Result<&'static RegistryDb, String> {
    // Try to get existing instance
    if let Some(db) = REGISTRY_DB.get() {
        tracing::debug!("Using existing DB instance");
        return Ok(db);
    }

    // Initialize new instance
    tracing::debug!("Initializing DB instance");

    let home = dirs::home_dir()
        .ok_or_else(|| "Failed to get home directory".to_string())?;

    let db_path = home.join(".apx").join("db");
    tracing::debug!("DB path: {}", db_path.display());

    let config = DbConfig {
        db_path: db_path.to_string_lossy().to_string(),
        cache_ttl_hours: 1,
    };

    let db = RegistryDb::new(config).await?;

    // Try to set the OnceLock. If another thread beat us to it, use their instance
    match REGISTRY_DB.set(db) {
        Ok(()) => {
            tracing::debug!("DB instance initialized successfully");
            Ok(REGISTRY_DB.get().unwrap())
        }
        Err(_) => {
            tracing::debug!("Another thread initialized DB instance first");
            Ok(REGISTRY_DB.get().unwrap())
        }
    }
}

// Main database interface (simplified stub)
pub struct RegistryDb {
    _config: DbConfig,
}

impl RegistryDb {
    pub async fn new(config: DbConfig) -> Result<Self, String> {
        // For now, just create the struct without actual initialization
        // This allows the code to compile while we work on the full implementation
        Ok(Self {
            _config: config,
        })
    }

    // Get registry catalog with caching (stub)
    pub async fn get_catalog(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<RegistryCatalogEntry>, String> {
        tracing::debug!("Fetching registry catalog");
        // Direct HTTP fetch (inlined to avoid circular dependency)
        let url = "https://ui.shadcn.com/r/registries.json";
        let result = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch registry catalog: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Registry catalog returned error: {e}"))?
            .json::<Vec<RegistryCatalogEntry>>()
            .await
            .map_err(|e| format!("Invalid registry catalog JSON: {e}"));

        match &result {
            Ok(catalog) => tracing::debug!("Fetched {} registry entries", catalog.len()),
            Err(e) => tracing::debug!("Failed to fetch registry catalog: {}", e),
        }

        result
    }

    // Get component with caching (stub)
    pub async fn get_component(
        &self,
        client: &reqwest::Client,
        req: &ResolvedRequest,
        registry_name: Option<&str>,
        _style: &str,
        component_name: &str,
    ) -> Result<(RegistryItem, Vec<String>), String> {
        tracing::debug!(
            "Fetching component '{}' from registry '{}'",
            component_name,
            registry_name.unwrap_or("default")
        );

        // Fallback to direct HTTP fetch for now (avoids circular dependency)
        let result = match req.url.scheme() {
            "http" | "https" => crate::cli::components::fetch_http_component(client, req).await,
            "file" => Err("File URLs not supported in cache yet".to_string()),
            scheme => Err(format!("Unsupported registry URL scheme: {scheme}")),
        };

        match &result {
            Ok((item, warnings)) => tracing::debug!(
                "Fetched component '{}' with {} files and {} warnings",
                component_name,
                item.files.len(),
                warnings.len()
            ),
            Err(e) => tracing::debug!("Failed to fetch component '{}': {}", component_name, e),
        }

        result
    }

    // Search components with vector similarity (stub)
    pub async fn search_components(
        &self,
        query: &str,
        limit: usize,
        categories: Option<&[String]>,
        item_types: Option<&[String]>,
        registries: Option<&[String]>,
    ) -> Result<Vec<SearchResult>, String> {
        tracing::debug!(
            "Searching components: query='{}', limit={}, categories={:?}, types={:?}, registries={:?}",
            query,
            limit,
            categories,
            item_types,
            registries
        );

        // Return empty results for now
        // Full implementation would use embeddings and vector search
        let results = vec![];
        tracing::debug!("Search returned {} results", results.len());
        Ok(results)
    }
}
