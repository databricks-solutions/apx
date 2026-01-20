use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use super::models::{RegistryItem, RegistryCatalogEntry, ComponentsJson};
use super::{resolve_component_request, fetch_component_impl, merge_registries, fetch_registry_catalog_impl};

/// Current cache format version
const CACHE_VERSION: u8 = 2;

/// Cached component item
#[derive(Debug, Serialize, Deserialize)]
struct CachedItem {
    version: u8,
    fetched_at: i64,
    item: RegistryItem,
    warnings: Vec<String>,
}

/// Registry catalog cache
#[derive(Debug, Serialize, Deserialize)]
struct CachedRegistryCatalog {
    version: u8,
    fetched_at: i64,
    entries: Vec<RegistryCatalogEntry>,
}

/// Get the base cache directory path (~/.apx/cache/components/)
fn get_cache_base_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "Could not determine home directory".to_string())?;

    Ok(home.join(".apx").join("cache").join("components"))
}

/// Get the registry items directory path
fn get_items_dir() -> Result<PathBuf, String> {
    Ok(get_cache_base_dir()?.join("items"))
}

/// Get the registries.json cache file path
fn get_registries_cache_path() -> Result<PathBuf, String> {
    Ok(get_cache_base_dir()?.join("registries.json"))
}

/// Get the directory for a specific registry's items
/// Default registry maps to "ui", others use their name (e.g., "@animate-ui")
fn get_registry_items_dir(registry_name: Option<&str>) -> Result<PathBuf, String> {
    let registry_dir = match registry_name {
        None => "ui".to_string(), // Default shadcn
        Some(name) => name.to_string(), // Keep full name like "@animate-ui"
    };

    Ok(get_items_dir()?.join(registry_dir))
}

/// Get the path for a specific component cache file
fn get_component_cache_path(component_name: &str, registry_name: Option<&str>) -> Result<PathBuf, String> {
    let registry_dir = get_registry_items_dir(registry_name)?;
    let filename = format!("{}.json", component_name);
    Ok(registry_dir.join(filename))
}

/// Check if a cache entry is still fresh
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
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
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
        .map_err(|e| format!("Failed to serialize cache: {}", e))?;

    // Write to temporary file first
    let temp_path = cache_path.with_extension("tmp");

    fs::write(&temp_path, json_content)
        .map_err(|e| format!("Failed to write cache file: {}", e))?;

    // Atomic rename
    fs::rename(&temp_path, &cache_path)
        .map_err(|e| format!("Failed to rename cache file: {}", e))?;

    Ok(())
}

/// Load cached registry catalog
pub fn load_cached_registry_catalog() -> Result<Option<Vec<RegistryCatalogEntry>>, String> {
    let cache_path = match get_registries_cache_path() {
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

    let cached: CachedRegistryCatalog = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    if cached.version != CACHE_VERSION {
        return Ok(None);
    }

    // Registry catalog uses longer TTL (24 hours)
    if !is_cache_fresh(cached.fetched_at, 24) {
        return Ok(None);
    }

    Ok(Some(cached.entries))
}

/// Save registry catalog to cache
pub fn save_cached_registry_catalog(entries: &[RegistryCatalogEntry]) -> Result<(), String> {
    let cache_path = get_registries_cache_path()?;

    if let Some(cache_dir) = cache_path.parent() {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
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

    let json_content = serde_json::to_string_pretty(&cached)
        .map_err(|e| format!("Failed to serialize registry catalog cache: {}", e))?;

    let temp_path = cache_path.with_extension("tmp");

    fs::write(&temp_path, json_content)
        .map_err(|e| format!("Failed to write registry catalog cache file: {}", e))?;

    fs::rename(&temp_path, &cache_path)
        .map_err(|e| format!("Failed to rename registry catalog cache file: {}", e))?;

    Ok(())
}

/// List all available components for a registry from the shadcn index
/// This fetches the components list from the registry's index endpoint
async fn fetch_registry_components_list(
    client: &reqwest::Client,
    registry_name: Option<&str>,
    _cfg: &ComponentsJson,
) -> Result<Vec<String>, String> {
    // For default shadcn registry, we need to fetch the components list
    // The shadcn registry has an index at https://ui.shadcn.com/r/index.json
    if registry_name.is_none() {
        let index_url = "https://ui.shadcn.com/r/index.json";

        let response = client
            .get(index_url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch registry index: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Registry index returned error: {e}"))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| format!("Invalid registry index JSON: {e}"))?;

        // Extract component names from the index
        // The index is an array of objects with "name" fields
        if let Some(arr) = response.as_array() {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|item| item.get("name")?.as_str().map(|s| s.to_string()))
                .collect();

            if !names.is_empty() {
                return Ok(names);
            }
        }

        // Fallback to a known list of common components if index fetch fails
        return Ok(vec![
            "accordion", "alert", "alert-dialog", "aspect-ratio", "avatar",
            "badge", "button", "calendar", "card", "carousel", "checkbox",
            "collapsible", "command", "context-menu", "dialog", "drawer",
            "dropdown-menu", "form", "hover-card", "input", "label",
            "menubar", "navigation-menu", "pagination", "popover", "progress",
            "radio-group", "resizable", "scroll-area", "select", "separator",
            "sheet", "skeleton", "slider", "sonner", "switch", "table",
            "tabs", "textarea", "toast", "toggle", "toggle-group", "tooltip",
        ].iter().map(|s| s.to_string()).collect());
    }

    // For custom registries, we'd need to fetch their index
    // For now, we'll return an empty list since we don't know their structure
    // They will be populated as components are requested
    tracing::warn!(
        "Custom registry {:?} doesn't have a known index endpoint",
        registry_name
    );
    Ok(Vec::new())
}

/// Sync all items from all registries in components.json
/// This downloads ALL items from each registry and caches them
pub async fn sync_all_registries(
    app_dir: &Path,
) -> Result<HashMap<String, Vec<String>>, String> {
    tracing::info!("Syncing all registries from components.json");

    let cfg = super::load_components_json(app_dir)?.config;
    let client = reqwest::Client::new();

    // Fetch registry catalog
    let discovered = fetch_registry_catalog_impl(&client).await?;
    let merged_registries = merge_registries(&cfg.registries, &discovered);

    let merged_cfg = ComponentsJson {
        style: cfg.style.clone(),
        aliases: cfg.aliases.clone(),
        registries: merged_registries.clone(),
        tailwind: cfg.tailwind.clone(),
    };

    let mut synced_items: HashMap<String, Vec<String>> = HashMap::new();

    // Sync default shadcn registry
    tracing::info!("Syncing default shadcn registry");
    match fetch_registry_components_list(&client, None, &merged_cfg).await {
        Ok(component_names) => {
            tracing::info!("Found {} components in default registry", component_names.len());

            let mut successful = Vec::new();
            for component_name in &component_names {
                match resolve_component_request(&merged_cfg, None, component_name) {
                    Ok(req) => {
                        match fetch_component_impl(&client, &req, None, Some(component_name)).await {
                            Ok((item, warnings)) => {
                                if let Err(e) = save_cached_component(component_name, None, &item, &warnings) {
                                    tracing::warn!("Failed to cache component {}: {}", component_name, e);
                                } else {
                                    successful.push(component_name.clone());
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to fetch component {}: {}", component_name, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to resolve component {}: {}", component_name, e);
                    }
                }
            }

            tracing::info!("Successfully synced {} components from default registry", successful.len());
            synced_items.insert("default".to_string(), successful);
        }
        Err(e) => {
            tracing::warn!("Failed to fetch components list for default registry: {}", e);
        }
    }

    // For custom registries, we can't easily get a list of all components
    // They will be populated as components are requested
    for (registry_name, _) in &merged_registries {
        tracing::debug!("Registry {} will be synced on-demand", registry_name);
    }

    Ok(synced_items)
}

/// Get all cached components from all registries
/// Returns a map of registry_name -> Vec<(component_name, RegistryItem)>
pub fn get_all_cached_components() -> Result<HashMap<String, Vec<(String, RegistryItem)>>, String> {
    let items_dir = get_items_dir()?;

    if !items_dir.exists() {
        return Ok(HashMap::new());
    }

    let mut result: HashMap<String, Vec<(String, RegistryItem)>> = HashMap::new();

    // Iterate through registry directories
    let registry_dirs = fs::read_dir(&items_dir)
        .map_err(|e| format!("Failed to read items directory: {}", e))?;

    for registry_entry in registry_dirs {
        let registry_entry = match registry_entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let registry_path = registry_entry.path();
        if !registry_path.is_dir() {
            continue;
        }

        let registry_name = registry_entry.file_name().to_string_lossy().to_string();
        let registry_key = if registry_name == "ui" {
            "default".to_string()
        } else {
            registry_name.clone()
        };

        let mut components = Vec::new();

        // Iterate through component files
        let component_files = match fs::read_dir(&registry_path) {
            Ok(files) => files,
            Err(_) => continue,
        };

        for component_entry in component_files {
            let component_entry = match component_entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let component_path = component_entry.path();
            if !component_path.is_file() || component_path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }

            let component_name = component_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Load the cached component
            let registry_arg = if registry_key == "default" {
                None
            } else {
                Some(registry_key.as_str())
            };

            if let Ok(Some((item, _warnings))) = load_cached_component(&component_name, registry_arg) {
                components.push((component_name, item));
            }
        }

        if !components.is_empty() {
            result.insert(registry_key, components);
        }
    }

    Ok(result)
}
