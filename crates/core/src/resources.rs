use rust_embed::RustEmbed;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(RustEmbed, Debug)]
#[folder = "../../src/apx/templates/"]
#[exclude = "__pycache__/*"]
#[exclude = "*.pyc"]
pub struct Templates;

#[derive(RustEmbed, Debug)]
#[folder = "../../src/apx/assets/"]
pub struct Assets;

/// Extract all embedded templates to a destination directory.
pub fn extract_templates_to(dest: &Path) -> Result<(), String> {
    for file in Templates::iter() {
        let data = Templates::get(&file)
            .ok_or_else(|| format!("Failed to read embedded template: {file}"))?;
        let file_path: &str = &file;
        let target = dest.join(file_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create dir {}: {e}", parent.display()))?;
        }
        fs::write(&target, data.data.as_ref())
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;
    }
    Ok(())
}

/// Get the content of the frontend entrypoint.ts asset.
pub fn entrypoint_ts_content() -> Result<Vec<u8>, String> {
    let data = Assets::get("entrypoint.ts")
        .ok_or_else(|| "entrypoint.ts not found in embedded assets".to_string())?;
    Ok(data.data.to_vec())
}

/// Write entrypoint.ts to a cache directory, returning the path.
/// Uses hash-based invalidation: only writes if content changed.
pub fn materialize_entrypoint_ts() -> Result<PathBuf, String> {
    let cache_dir = dirs::home_dir()
        .ok_or("Could not determine home directory")?
        .join(".apx")
        .join("cache");

    fs::create_dir_all(&cache_dir).map_err(|e| format!("Failed to create cache dir: {e}"))?;

    let dest = cache_dir.join("entrypoint.ts");
    let content = entrypoint_ts_content()?;

    // Only write if content differs (hash-based invalidation)
    let needs_write = if dest.exists() {
        let existing =
            fs::read(&dest).map_err(|e| format!("Failed to read existing entrypoint.ts: {e}"))?;
        existing != content
    } else {
        true
    };

    if needs_write {
        fs::write(&dest, &content).map_err(|e| format!("Failed to write entrypoint.ts: {e}"))?;
    }

    Ok(dest)
}
