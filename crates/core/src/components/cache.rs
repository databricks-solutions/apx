use super::models::{RegistryCatalogEntry, RegistryConfig, RegistryItem, UiConfig};
use crate::common::read_project_metadata;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::warn;

/// Current cache format version
const CACHE_VERSION: u8 = 2;

/// Cache TTL in hours
const CACHE_TTL_HOURS: i64 = 1;

/// Retry configuration for HTTP requests
const MAX_RETRIES: u32 = 5;
const INITIAL_DELAY_MS: u64 = 125;

/// Execute an async operation with exponential backoff retry.
///
/// Retries up to 5 times with delays: 125ms, 250ms, 500ms, 1000ms, 2000ms (~4 seconds total).
async fn fetch_with_retry<T, F, Fut>(operation: F, operation_name: &str) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_error = String::new();
    for attempt in 0..MAX_RETRIES {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt < MAX_RETRIES - 1 {
                    let delay = INITIAL_DELAY_MS * (1 << attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        delay_ms = delay,
                        operation = operation_name,
                        error = %last_error,
                        "HTTP request failed, retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }
    Err(format!(
        "{operation_name}: {last_error} (after {MAX_RETRIES} retries)"
    ))
}

/// Cached component item
#[derive(Debug, Serialize, Deserialize)]
struct CachedItem {
    version: u8,
    fetched_at: i64,
    item: RegistryItem,
    warnings: Vec<String>,
}

/// Registry catalog cache (shadcn directory)
#[derive(Debug, Serialize, Deserialize)]
struct CachedRegistryCatalog {
    version: u8,
    fetched_at: i64,
    entries: Vec<RegistryCatalogEntry>,
}

/// Cached registry index (registry.json content)
#[derive(Debug, Serialize, Deserialize)]
struct CachedRegistryIndex {
    version: u8,
    fetched_at: i64,
    items: Vec<RegistryIndexItem>,
}

/// Item from registry.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndexItem {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default, rename = "registryDependencies")]
    pub registry_dependencies: Vec<String>,
}

/// Get the base cache directory path
///
/// Uses APX_CACHE_DIR environment variable if set, otherwise defaults to ~/.apx/cache.
/// Returns the path to the components subdirectory.
fn get_cache_base_dir() -> Result<PathBuf, String> {
    let cache_root = if let Ok(cache_dir) = std::env::var("APX_CACHE_DIR") {
        PathBuf::from(cache_dir)
    } else {
        let home =
            dirs::home_dir().ok_or_else(|| "Could not determine home directory".to_string())?;
        home.join(".apx").join("cache")
    };
    Ok(cache_root.join("components"))
}

/// Get the registry items directory path
fn get_items_dir() -> Result<PathBuf, String> {
    Ok(get_cache_base_dir()?.join("items"))
}

/// Get the registries directory path (for registry.json files)
fn get_registries_dir() -> Result<PathBuf, String> {
    Ok(get_cache_base_dir()?.join("registries"))
}

/// Get the registries.json cache file path (shadcn directory)
fn get_registries_catalog_path() -> Result<PathBuf, String> {
    Ok(get_cache_base_dir()?.join("registries.json"))
}

/// Get registry directory name
fn registry_dir_name(registry_name: Option<&str>) -> String {
    match registry_name {
        None => "ui".to_string(),
        Some(name) => name.trim_start_matches('@').to_string(),
    }
}

/// Get the path for a registry's registry.json cache
fn get_registry_index_path(registry_name: Option<&str>) -> Result<PathBuf, String> {
    let dir_name = registry_dir_name(registry_name);
    Ok(get_registries_dir()?.join(&dir_name).join("registry.json"))
}

/// Get the directory for a specific registry's items
fn get_registry_items_dir(registry_name: Option<&str>) -> Result<PathBuf, String> {
    let dir_name = registry_dir_name(registry_name);
    Ok(get_items_dir()?.join(&dir_name))
}

/// Get the path for a specific component cache file
fn get_component_cache_path(
    component_name: &str,
    registry_name: Option<&str>,
) -> Result<PathBuf, String> {
    let registry_dir = get_registry_items_dir(registry_name)?;
    let filename = format!("{component_name}.json");
    Ok(registry_dir.join(filename))
}

/// Check if a cache file is fresh based on mtime
fn is_file_fresh(path: &Path, ttl_hours: i64) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return false;
    };
    elapsed.as_secs() < (ttl_hours * 3600) as u64
}

/// Check if a cache entry is still fresh (by fetched_at timestamp)
fn is_cache_fresh(fetched_at: i64, ttl_hours: i64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ttl_seconds = ttl_hours * 3600;
    now - fetched_at < ttl_seconds
}

/// Load a cached component from disk
pub fn load_cached_component(
    component_name: &str,
    registry_name: Option<&str>,
) -> Result<Option<(RegistryItem, Vec<String>)>, String> {
    let cache_path = match get_component_cache_path(component_name, registry_name) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };

    if !cache_path.exists() {
        return Ok(None);
    }

    let content = match fs::read_to_string(&cache_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let cached: CachedItem = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // Validate cache version
    if cached.version != CACHE_VERSION {
        return Ok(None);
    }

    // Check if cache is fresh (24 hour TTL for registry items)
    if !is_cache_fresh(cached.fetched_at, 24) {
        return Ok(None);
    }

    Ok(Some((cached.item, cached.warnings)))
}

/// Save a component to the cache
pub fn save_cached_component(
    component_name: &str,
    registry_name: Option<&str>,
    item: &RegistryItem,
    warnings: &[String],
) -> Result<(), String> {
    let cache_path = get_component_cache_path(component_name, registry_name)?;

    // Ensure parent directory exists
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create cache directory: {e}"))?;
    }

    // Get current timestamp
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Create cached item
    let cached = CachedItem {
        version: CACHE_VERSION,
        fetched_at: now,
        item: item.clone(),
        warnings: warnings.to_vec(),
    };

    // Serialize to JSON
    let json_content = serde_json::to_string_pretty(&cached)
        .map_err(|e| format!("Failed to serialize cache: {e}"))?;

    // Write to temporary file first
    let temp_path = cache_path.with_extension("tmp");

    fs::write(&temp_path, json_content).map_err(|e| format!("Failed to write cache file: {e}"))?;

    // Atomic rename
    fs::rename(&temp_path, &cache_path).map_err(|e| format!("Failed to rename cache file: {e}"))?;

    Ok(())
}

/// Load cached registry catalog (shadcn directory)
pub fn load_cached_registry_catalog() -> Result<Option<Vec<RegistryCatalogEntry>>, String> {
    let cache_path = get_registries_catalog_path()?;
    if !is_file_fresh(&cache_path, CACHE_TTL_HOURS) {
        return Ok(None);
    }
    let content = fs::read_to_string(&cache_path).map_err(|e| e.to_string())?;
    let cached: CachedRegistryCatalog =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;
    if cached.version != CACHE_VERSION {
        return Ok(None);
    }
    Ok(Some(cached.entries))
}

/// Save registry catalog to cache
pub fn save_cached_registry_catalog(entries: &[RegistryCatalogEntry]) -> Result<(), String> {
    let cache_path = get_registries_catalog_path()?;
    if let Some(cache_dir) = cache_path.parent() {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {e}"))?;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cached = CachedRegistryCatalog {
        version: CACHE_VERSION,
        fetched_at: now,
        entries: entries.to_vec(),
    };
    let json_content = serde_json::to_string_pretty(&cached).map_err(|e| e.to_string())?;
    let temp_path = cache_path.with_extension("tmp");
    fs::write(&temp_path, &json_content).map_err(|e| e.to_string())?;
    fs::rename(&temp_path, &cache_path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Load cached registry index (registry.json)
fn load_cached_registry_index(
    registry_name: Option<&str>,
) -> Result<Option<Vec<RegistryIndexItem>>, String> {
    let cache_path = get_registry_index_path(registry_name)?;
    if !is_file_fresh(&cache_path, CACHE_TTL_HOURS) {
        return Ok(None);
    }
    let content = fs::read_to_string(&cache_path).map_err(|e| e.to_string())?;
    let cached: CachedRegistryIndex = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    if cached.version != CACHE_VERSION {
        return Ok(None);
    }
    Ok(Some(cached.items))
}

/// Save registry index to cache
fn save_cached_registry_index(
    registry_name: Option<&str>,
    items: &[RegistryIndexItem],
) -> Result<(), String> {
    let cache_path = get_registry_index_path(registry_name)?;
    if let Some(cache_dir) = cache_path.parent() {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {e}"))?;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cached = CachedRegistryIndex {
        version: CACHE_VERSION,
        fetched_at: now,
        items: items.to_vec(),
    };
    let json_content = serde_json::to_string_pretty(&cached).map_err(|e| e.to_string())?;
    let temp_path = cache_path.with_extension("tmp");
    fs::write(&temp_path, &json_content).map_err(|e| e.to_string())?;
    fs::rename(&temp_path, &cache_path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Get registry.json URL for a registry
/// Default: https://ui.shadcn.com/r/styles/new-york/registry.json
/// Custom: replace {name} with "registry" in template
fn get_registry_index_url(
    registry_name: Option<&str>,
    registry_config: Option<&RegistryConfig>,
    style: &str,
) -> Option<String> {
    match (registry_name, registry_config) {
        (None, _) => Some(format!(
            "https://ui.shadcn.com/r/styles/{style}/registry.json"
        )),
        (Some(_), Some(config)) => {
            let template = match config {
                RegistryConfig::Template(t) => t.clone(),
                RegistryConfig::Advanced(a) => a.url.clone(),
            };
            // Replace {name} with "registry" and remove {style} if present
            let url = template
                .replace("{name}", "registry")
                .replace("{style}", style);
            Some(url)
        }
        _ => None,
    }
}

/// Fetch and cache registry index (registry.json)
async fn fetch_and_cache_registry_index(
    client: &reqwest::Client,
    registry_name: Option<&str>,
    registry_config: Option<&RegistryConfig>,
    style: &str,
) -> Result<Vec<RegistryIndexItem>, String> {
    // Check cache first
    if let Ok(Some(items)) = load_cached_registry_index(registry_name) {
        tracing::debug!("Using cached registry index for {:?}", registry_name);
        return Ok(items);
    }

    let url = get_registry_index_url(registry_name, registry_config, style)
        .ok_or_else(|| format!("Cannot determine registry URL for {registry_name:?}"))?;

    tracing::debug!("Fetching registry index from: {}", url);

    // HTTP fetch with retry
    let json_value: serde_json::Value = fetch_with_retry(
        || {
            let url = url.clone();
            async move {
                client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("Failed to fetch registry index {url}: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("Registry index {url} returned error: {e}"))?
                    .json::<serde_json::Value>()
                    .await
                    .map_err(|e| format!("Invalid JSON from registry index {url}: {e}"))
            }
        },
        &format!("fetch registry index from {url}"),
    )
    .await?;

    // Parse items - can be at root level as array or in "items" field
    let items_array = json_value
        .get("items")
        .and_then(|v| v.as_array())
        .or_else(|| json_value.as_array());

    let items: Vec<RegistryIndexItem> = match items_array {
        Some(arr) => arr
            .iter()
            .filter_map(|item| {
                let name = item.get("name")?.as_str()?.to_string();
                Some(RegistryIndexItem {
                    name,
                    description: item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    dependencies: item
                        .get("dependencies")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    registry_dependencies: item
                        .get("registryDependencies")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect(),
        None => Vec::new(),
    };

    if !items.is_empty() {
        let _ = save_cached_registry_index(registry_name, &items);
        tracing::debug!(
            "Cached {} items from registry {:?}",
            items.len(),
            registry_name.unwrap_or("default")
        );
    }

    Ok(items)
}

/// Check if any registry.json files need refresh (older than 1 hour)
pub fn needs_registry_refresh(registries: &HashMap<String, RegistryConfig>) -> bool {
    // Check default registry
    if let Ok(path) = get_registry_index_path(None)
        && !is_file_fresh(&path, CACHE_TTL_HOURS)
    {
        return true;
    }
    // Check custom registries
    for registry_name in registries.keys() {
        if let Ok(path) = get_registry_index_path(Some(registry_name))
            && !is_file_fresh(&path, CACHE_TTL_HOURS)
        {
            return true;
        }
    }
    false
}

/// Sync registry.json files only (not individual items, except default shadcn)
/// Returns true if any registry was refreshed
pub async fn sync_registry_indexes(app_dir: &Path, force: bool) -> Result<bool, String> {
    let metadata = read_project_metadata(app_dir)?;
    let cfg = UiConfig::from_metadata(&metadata, app_dir);
    let client = reqwest::Client::new();
    let style = cfg.style();
    let mut refreshed = false;

    // Sync default registry index (registry.json only, individual items fetched on-demand)
    let default_path = get_registry_index_path(None)?;
    if force || !is_file_fresh(&default_path, CACHE_TTL_HOURS) {
        tracing::debug!("Fetching default registry index");
        match fetch_and_cache_registry_index(&client, None, None, style).await {
            Ok(items) => {
                tracing::debug!("Cached {} items in default registry index", items.len());
                refreshed = true;
            }
            Err(e) => tracing::warn!("Failed to fetch default registry index: {}", e),
        }
    }

    // Sync custom registry indexes (registry.json only, no item prefetch)
    for (registry_name, registry_config) in &cfg.registries {
        let path = get_registry_index_path(Some(registry_name))?;
        if force || !is_file_fresh(&path, CACHE_TTL_HOURS) {
            tracing::debug!("Fetching registry index for {}", registry_name);
            match fetch_and_cache_registry_index(
                &client,
                Some(registry_name),
                Some(registry_config),
                style,
            )
            .await
            {
                Ok(items) => {
                    tracing::debug!(
                        "Cached {} items in {} registry index",
                        items.len(),
                        registry_name
                    );
                    refreshed = true;
                }
                Err(e) => tracing::warn!("Failed to fetch {} registry index: {}", registry_name, e),
            }
        }
    }

    Ok(refreshed)
}

/// Get all cached registry indexes for building search index
pub fn get_all_registry_indexes() -> Result<HashMap<String, Vec<RegistryIndexItem>>, String> {
    let registries_dir = get_registries_dir()?;
    if !registries_dir.exists() {
        return Ok(HashMap::new());
    }

    let mut result = HashMap::new();

    for entry in fs::read_dir(&registries_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let registry_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let index_path = path.join("registry.json");
        if !index_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&index_path).map_err(|e| e.to_string())?;
        let cached: CachedRegistryIndex =
            serde_json::from_str(&content).map_err(|e| e.to_string())?;

        result.insert(registry_name, cached.items);
    }

    Ok(result)
}

/// State for tracking background indexing
#[derive(Debug, Clone, Default)]
pub struct CachePopulationState {
    pub is_running: bool,
}

/// Shared state for cache population
pub type SharedCacheState = Arc<Mutex<CachePopulationState>>;

/// Create a new shared cache state
pub fn new_cache_state() -> SharedCacheState {
    Arc::new(Mutex::new(CachePopulationState::default()))
}
