use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use super::models::RegistryItem;

/// Cached component data structure
#[derive(Debug, Serialize, Deserialize)]
struct CachedComponent {
    version: u8,
    component_name: String,
    registry_name: Option<String>,
    fetched_at: i64,
    item: RegistryItem,
    warnings: Vec<String>,
}

/// Current cache format version
const CACHE_VERSION: u8 = 1;

/// Get the cache directory path (~/.apx/cache/components/)
fn get_cache_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "Could not determine home directory".to_string())?;

    Ok(home.join(".apx").join("cache").join("components"))
}

/// Sanitize a string for use in a filename
/// Replaces unsafe characters with underscores
fn sanitize_filename_part(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '@' => '_',
            _ => c,
        })
        .collect()
}

/// Generate cache filename for a component
fn get_cache_filename(component_name: &str, registry_name: Option<&str>) -> String {
    let registry = registry_name.unwrap_or("default");
    let sanitized_registry = sanitize_filename_part(registry);
    let sanitized_component = sanitize_filename_part(component_name);
    format!("{}_{}.json", sanitized_registry, sanitized_component)
}

/// Check if a cache entry is still fresh
pub fn is_cache_fresh(fetched_at: i64, ttl_hours: i64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let ttl_seconds = ttl_hours * 3600;
    now - fetched_at < ttl_seconds
}

/// Load a cached component from disk
/// Returns Ok(Some((item, warnings))) if cache exists and is valid
/// Returns Ok(None) if cache doesn't exist, is corrupted, or is stale
/// This function is designed to be non-fatal - any errors result in Ok(None)
pub fn load_cached_component(
    component_name: &str,
    registry_name: Option<&str>,
) -> Result<Option<(RegistryItem, Vec<String>)>, String> {
    // Get cache directory - if this fails, just return None
    let cache_dir = match get_cache_dir() {
        Ok(dir) => dir,
        Err(_) => return Ok(None),
    };

    let filename = get_cache_filename(component_name, registry_name);
    let cache_path = cache_dir.join(&filename);

    // Check if file exists
    if !cache_path.exists() {
        return Ok(None);
    }

    // Try to read and parse the cache file
    let content = match fs::read_to_string(&cache_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let cached: CachedComponent = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // Validate cache version
    if cached.version != CACHE_VERSION {
        return Ok(None);
    }

    // Check if cache is fresh (default 1 hour TTL)
    if !is_cache_fresh(cached.fetched_at, 1) {
        return Ok(None);
    }

    Ok(Some((cached.item, cached.warnings)))
}

/// Save a component to the cache
/// Errors are returned but should generally be treated as non-fatal
pub fn save_cached_component(
    component_name: &str,
    registry_name: Option<&str>,
    item: &RegistryItem,
    warnings: &[String],
) -> Result<(), String> {
    // Get cache directory
    let cache_dir = get_cache_dir()?;

    // Ensure cache directory exists
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache directory: {}", e))?;

    let filename = get_cache_filename(component_name, registry_name);
    let final_path = cache_dir.join(&filename);

    // Get current timestamp
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Create cached component
    let cached = CachedComponent {
        version: CACHE_VERSION,
        component_name: component_name.to_string(),
        registry_name: registry_name.map(|s| s.to_string()),
        fetched_at: now,
        item: item.clone(),
        warnings: warnings.to_vec(),
    };

    // Serialize to JSON
    let json_content = serde_json::to_string_pretty(&cached)
        .map_err(|e| format!("Failed to serialize cache: {}", e))?;

    // Write to temporary file first
    let temp_filename = format!("{}.tmp.{}", filename, uuid::Uuid::new_v4().simple());
    let temp_path = cache_dir.join(&temp_filename);

    fs::write(&temp_path, json_content)
        .map_err(|e| format!("Failed to write cache file: {}", e))?;

    // Atomic rename
    fs::rename(&temp_path, &final_path)
        .map_err(|e| format!("Failed to rename cache file: {}", e))?;

    Ok(())
}
