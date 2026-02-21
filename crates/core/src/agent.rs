//! Agent binary management module.
//!
//! The agent binary is embedded in the apx binary via `include_bytes!` and
//! extracted to `~/.apx/apx-agent` on first use. Version management is handled
//! by always overwriting with the embedded copy (which matches the apx version).

use std::path::PathBuf;

#[cfg(target_os = "windows")]
const AGENT_FILENAME: &str = "apx-agent.exe";
#[cfg(not(target_os = "windows"))]
const AGENT_FILENAME: &str = "apx-agent";

/// Get path to installed agent in ~/.apx/
pub fn installed_agent_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx").join(AGENT_FILENAME))
}

/// Ensure agent binary is installed and up-to-date.
///
/// Delegates to `resolve_apx_agent_binary_path()` which extracts the embedded
/// agent binary to `~/.apx/apx-agent` (always overwriting to match versions).
pub fn ensure_installed() -> Result<PathBuf, String> {
    crate::interop::resolve_apx_agent_binary_path()
}
