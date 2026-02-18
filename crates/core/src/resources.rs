use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;

use rust_embed::RustEmbed;

/// Embedded templates from `src/apx/templates/`.
#[derive(RustEmbed)]
#[folder = "../../src/apx/templates"]
struct Templates;

/// Embedded entrypoint.ts content.
const ENTRYPOINT_TS: &str = include_str!("../../../src/apx/assets/entrypoint.ts");

/// Agent binary — copied to OUT_DIR by build.rs.
const AGENT_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/apx-agent"));

/// Get the content of an embedded template file.
///
/// The path is relative to `src/apx/templates/`, e.g. `"base/pyproject.toml.jinja2"`.
pub fn get_template(path: &str) -> Option<Cow<'static, [u8]>> {
    Templates::get(path).map(|f| f.data)
}

/// Get the content of an embedded template file as a UTF-8 string.
pub fn get_template_str(path: &str) -> Option<Cow<'static, str>> {
    let data = Templates::get(path)?;
    match data.data {
        Cow::Borrowed(bytes) => {
            // In release mode, rust_embed returns borrowed slices
            std::str::from_utf8(bytes).ok().map(Cow::Borrowed)
        }
        Cow::Owned(bytes) => {
            // In debug mode, rust_embed reads from disk
            String::from_utf8(bytes).ok().map(Cow::Owned)
        }
    }
}

/// List all embedded template files, optionally filtered by a path prefix.
pub fn list_templates(prefix: Option<&str>) -> Vec<String> {
    Templates::iter()
        .filter(|path| match prefix {
            Some(p) => path.starts_with(p),
            None => true,
        })
        .map(|path| path.to_string())
        .collect()
}

/// Write entrypoint.ts to `<project_root>/node_modules/.apx/entrypoint.ts`.
/// Always overwrites to ensure it matches the embedded version.
/// Returns the path to the written file.
pub fn ensure_entrypoint(project_root: &std::path::Path) -> Result<PathBuf, String> {
    let dir = project_root.join("node_modules").join(".apx");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let dest = dir.join("entrypoint.ts");
    fs::write(&dest, ENTRYPOINT_TS).map_err(|e| format!("Failed to write entrypoint.ts: {e}"))?;
    Ok(dest)
}

/// Root of the apx data directory: `~/.apx/`.
pub fn apx_home() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx"))
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
