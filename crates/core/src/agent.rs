//! Agent binary management module.
//!
//! The agent binary is embedded in the apx binary via `include_bytes!` and
//! extracted to `~/.apx/apx-agent` on first use. Version management is handled
//! by always overwriting with the embedded copy (which matches the apx version).

use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
const AGENT_FILENAME: &str = "apx-agent.exe";
#[cfg(not(target_os = "windows"))]
const AGENT_FILENAME: &str = "apx-agent";

/// Get path to installed agent in ~/.apx/
pub fn installed_agent_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx").join(AGENT_FILENAME))
}

/// Get version of installed agent by running `apx-agent --version`
pub fn installed_version() -> Option<String> {
    let path = installed_agent_path().ok()?;
    if !path.exists() {
        return None;
    }

    let output = Command::new(&path).arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let version_str = String::from_utf8(output.stdout).ok()?;
    // Parse "apx-agent 0.1.28" → "0.1.28"
    version_str.split_whitespace().nth(1).map(|s| s.to_string())
}

/// Ensure agent binary is installed and up-to-date.
///
/// Delegates to `resolve_apx_agent_binary_path()` which extracts the embedded
/// agent binary to `~/.apx/apx-agent` (always overwriting to match versions).
pub fn ensure_installed() -> Result<PathBuf, String> {
    crate::interop::resolve_apx_agent_binary_path()
}
