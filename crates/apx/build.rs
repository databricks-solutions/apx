//! Build script for apx-bin.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Workspace root is two levels up from crates/apx/
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or("Could not find workspace root")?;

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let output_dir = workspace_root.join("src/apx/binaries");
    fs::create_dir_all(&output_dir)?;

    // Clear old agent binaries
    for entry in fs::read_dir(&output_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("apx-agent") {
                fs::remove_file(&path)?;
            }
        }
    }

    // Copy Agent binary
    copy_agent_binary(workspace_root, &output_dir, &target_os, &target_arch)?;

    // Watch for changes
    let agent_dir = workspace_root.join(".bins/agent");
    println!("cargo:rerun-if-changed={}", agent_dir.display());

    Ok(())
}

fn copy_agent_binary(
    workspace_root: &Path,
    output_dir: &Path,
    target_os: &str,
    target_arch: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(agent_src_name) = agent_binary_name(target_os, target_arch) else {
        println!(
            "cargo:warning=Agent binary not available for {target_os}-{target_arch}, skipping"
        );
        return Ok(());
    };

    let agent_source = workspace_root.join(".bins/agent").join(agent_src_name);
    if !agent_source.exists() {
        println!(
            "cargo:warning=Agent binary not found at {}, skipping",
            agent_source.display()
        );
        return Ok(());
    }

    let agent_dest_name = if target_os == "windows" {
        "apx-agent.exe"
    } else {
        "apx-agent"
    };
    let agent_dest = output_dir.join(agent_dest_name);
    fs::copy(&agent_source, &agent_dest)?;
    set_executable_permissions(&agent_dest)?;
    println!("cargo:rerun-if-changed={}", agent_source.display());

    Ok(())
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

fn agent_binary_name(target_os: &str, target_arch: &str) -> Option<&'static str> {
    match (target_os, target_arch) {
        ("macos", "aarch64") => Some("apx-agent-darwin-aarch64"),
        ("macos", "x86_64") => Some("apx-agent-darwin-x64"),
        ("linux", "aarch64") => Some("apx-agent-linux-aarch64"),
        ("linux", "x86_64") => Some("apx-agent-linux-x64"),
        ("windows", "x86_64") => Some("apx-agent-windows-x64.exe"),
        _ => None,
    }
}
