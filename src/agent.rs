//! Agent binary management module.
//!
//! This module handles version checking and installation of the apx-agent binary.
//! The agent is bundled with the apx package and installed to ~/.apx/ on first use.

use std::path::PathBuf;
use std::process::Command;
use tracing::info;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(target_os = "windows")]
const AGENT_FILENAME: &str = "apx-agent.exe";
#[cfg(not(target_os = "windows"))]
const AGENT_FILENAME: &str = "apx-agent";

/// Get path to installed agent in ~/.apx/
pub fn installed_agent_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(".apx").join(AGENT_FILENAME))
}

/// Get path to bundled agent binary (via importlib.resources)
pub fn bundled_agent_path() -> Result<PathBuf, String> {
    pyo3::Python::attach(|py| {
        crate::interop::resolve_apx_agent_binary_path(py)
            .map_err(|e| format!("Failed to resolve bundled agent: {e}"))
    })
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

/// Ensure agent binary is installed and up-to-date
pub fn ensure_installed() -> Result<PathBuf, String> {
    let needs_install = match installed_version() {
        None => {
            info!("Flux agent not installed, will install");
            true
        }
        Some(v) if v != AGENT_VERSION => {
            info!(
                "Flux agent version mismatch (installed: {}, current: {}), will upgrade",
                v, AGENT_VERSION
            );
            true
        }
        Some(_) => false,
    };

    if needs_install {
        install_agent()?;
    }

    installed_agent_path()
}

/// Copy bundled agent to installation directory
fn install_agent() -> Result<(), String> {
    let bundled = bundled_agent_path()?;
    let installed = installed_agent_path()?;

    // Create .apx directory if needed
    if let Some(parent) = installed.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create .apx directory: {e}"))?;
    }

    // Copy binary
    std::fs::copy(&bundled, &installed).map_err(|e| format!("Failed to copy agent binary: {e}"))?;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed)
            .map_err(|e| format!("Failed to get file metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed, perms)
            .map_err(|e| format!("Failed to set executable permissions: {e}"))?;
    }

    info!("Flux agent installed to: {}", installed.display());
    Ok(())
}
