use std::fs;
use std::io::Read;
use std::path::PathBuf;

/// Zip archive of all assets (templates + entrypoint.ts) — produced by build.rs.
const ASSETS_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/assets.zip"));

/// Agent binary — copied to OUT_DIR by build.rs.
const AGENT_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/apx-agent"));

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

/// Ensure assets are extracted to the versioned directory.
/// Uses a `.extracted` sentinel file to skip if already done for this version.
/// Returns the versioned directory path.
pub fn ensure_extracted() -> Result<PathBuf, String> {
    let dir = versioned_dir()?;
    let sentinel = dir.join(".extracted");

    if sentinel.exists() {
        return Ok(dir);
    }

    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create dir {}: {e}", dir.display()))?;

    extract_archive(ASSETS_ARCHIVE, &dir)?;

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

/// Extract the embedded apx-agent binary to `~/.apx/apx-agent`.
/// Overwrites if existing version differs. Sets executable permissions on Unix.
pub fn ensure_agent_extracted() -> Result<PathBuf, String> {
    let apx_dir = apx_home()?;
    fs::create_dir_all(&apx_dir).map_err(|e| format!("Failed to create .apx dir: {e}"))?;

    #[cfg(target_os = "windows")]
    let agent_name = "apx-agent.exe";
    #[cfg(not(target_os = "windows"))]
    let agent_name = "apx-agent";

    let agent_dest = apx_dir.join(agent_name);

    // Always write to ensure version matches (the binary embeds its version)
    fs::write(&agent_dest, AGENT_BINARY)
        .map_err(|e| format!("Failed to write agent binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&agent_dest)
            .map_err(|e| format!("Failed to read agent metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&agent_dest, perms)
            .map_err(|e| format!("Failed to set agent permissions: {e}"))?;
    }

    Ok(agent_dest)
}

/// Extract a zip archive to `dest`, preserving internal directory structure.
fn extract_archive(data: &[u8], dest: &std::path::Path) -> Result<(), String> {
    let cursor = std::io::Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to open zip archive: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry {i}: {e}"))?;

        let name = entry.name().to_string();
        if name.ends_with('/') {
            // Directory entry — just create it
            let dir_path = dest.join(&name);
            fs::create_dir_all(&dir_path)
                .map_err(|e| format!("Failed to create dir {}: {e}", dir_path.display()))?;
            continue;
        }

        let target = dest.join(&name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create dir {}: {e}", parent.display()))?;
        }

        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| format!("Failed to read zip entry {name}: {e}"))?;
        fs::write(&target, &content)
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;
    }

    Ok(())
}
