use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Frontend entrypoint — known path, directly included.
const ENTRYPOINT_TS: &str = include_str!("../../../src/apx/assets/entrypoint.ts");

/// Archive of all templates — produced by build.rs.
const TEMPLATES_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/templates.pack"));

/// Bun binary — copied to OUT_DIR by build.rs.
const BUN_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bun"));

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root of the apx data directory: `~/.apx/`.
fn apx_home() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx"))
}

/// Versioned files directory: `~/.apx/files/<version>/`.
fn versioned_dir() -> Result<PathBuf, String> {
    Ok(apx_home()?.join("files").join(VERSION))
}

/// Ensure templates and entrypoint.ts are extracted to the versioned directory.
/// Uses a `.extracted` sentinel file to skip if already done for this version.
/// Returns the versioned directory path.
pub fn ensure_extracted() -> Result<PathBuf, String> {
    let dir = versioned_dir()?;
    let sentinel = dir.join(".extracted");

    if sentinel.exists() {
        return Ok(dir);
    }

    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create dir {}: {e}", dir.display()))?;

    // Extract templates
    let templates_dest = dir.join("templates");
    fs::create_dir_all(&templates_dest)
        .map_err(|e| format!("Failed to create templates dir: {e}"))?;
    unpack_archive(TEMPLATES_ARCHIVE, &templates_dest)?;

    // Write entrypoint.ts
    let entrypoint_dest = dir.join("entrypoint.ts");
    fs::write(&entrypoint_dest, ENTRYPOINT_TS)
        .map_err(|e| format!("Failed to write entrypoint.ts: {e}"))?;

    // Write sentinel
    fs::write(&sentinel, VERSION).map_err(|e| format!("Failed to write sentinel: {e}"))?;

    Ok(dir)
}

/// Returns the path to the extracted templates directory.
pub fn templates_dir() -> Result<PathBuf, String> {
    let dir = ensure_extracted()?;
    Ok(dir.join("templates"))
}

/// Returns the path to the extracted entrypoint.ts.
pub fn entrypoint_ts_path() -> Result<PathBuf, String> {
    let dir = ensure_extracted()?;
    Ok(dir.join("entrypoint.ts"))
}

/// Extract the embedded bun binary to `~/.apx/bin/bun`.
/// Skips if it already exists. Sets executable permissions on Unix.
pub fn ensure_bun_extracted() -> Result<PathBuf, String> {
    let bin_dir = apx_home()?.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| format!("Failed to create bin dir: {e}"))?;

    #[cfg(target_os = "windows")]
    let bun_name = "bun.exe";
    #[cfg(not(target_os = "windows"))]
    let bun_name = "bun";

    let bun_dest = bin_dir.join(bun_name);

    if bun_dest.exists() {
        return Ok(bun_dest);
    }

    fs::write(&bun_dest, BUN_BINARY).map_err(|e| format!("Failed to write bun binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bun_dest)
            .map_err(|e| format!("Failed to read bun metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bun_dest, perms)
            .map_err(|e| format!("Failed to set bun permissions: {e}"))?;
    }

    Ok(bun_dest)
}

/// Parse the simple binary archive format and write files to `dest`.
fn unpack_archive(data: &[u8], dest: &Path) -> Result<(), String> {
    let mut cursor = std::io::Cursor::new(data);

    let entry_count = read_u32_le(&mut cursor)?;

    for _ in 0..entry_count {
        let path_len = read_u32_le(&mut cursor)? as usize;
        let mut path_buf = vec![0u8; path_len];
        cursor
            .read_exact(&mut path_buf)
            .map_err(|e| format!("Failed to read path from archive: {e}"))?;
        let rel_path = String::from_utf8(path_buf)
            .map_err(|e| format!("Invalid UTF-8 path in archive: {e}"))?;

        let content_len = read_u32_le(&mut cursor)? as usize;
        let mut content = vec![0u8; content_len];
        cursor
            .read_exact(&mut content)
            .map_err(|e| format!("Failed to read content from archive: {e}"))?;

        let target = dest.join(&rel_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create dir {}: {e}", parent.display()))?;
        }
        fs::write(&target, &content)
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;
    }

    Ok(())
}

fn read_u32_le(cursor: &mut std::io::Cursor<&[u8]>) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| format!("Failed to read u32 from archive: {e}"))?;
    Ok(u32::from_le_bytes(buf))
}

/// Extract all embedded templates to a destination directory (used by tests or direct callers).
pub fn extract_templates_to(dest: &Path) -> Result<(), String> {
    unpack_archive(TEMPLATES_ARCHIVE, dest)
}

/// Get the content of the frontend entrypoint.ts asset.
pub fn entrypoint_ts_content() -> Result<Vec<u8>, String> {
    Ok(ENTRYPOINT_TS.as_bytes().to_vec())
}
