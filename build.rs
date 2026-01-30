use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BUN_BIN_DIR: &str = ".bins/bun";
const AGENT_BIN_DIR: &str = ".bins/agent";
const OUTPUT_DIR: &str = "src/apx/binaries";

fn main() {
    // protoc is expected to be available in PATH (installed via CI or locally)
    // See: arduino/setup-protoc in CI workflows

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let output_dir = manifest_dir.join(OUTPUT_DIR);
    fs::create_dir_all(&output_dir).expect("Failed to create binaries output dir");

    // Clear old binaries
    for entry in fs::read_dir(&output_dir).expect("Failed to read binaries output dir") {
        let entry = entry.expect("Failed to read binaries output entry");
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(&path).expect("Failed to remove old binary");
        }
    }

    // Copy Bun binary
    copy_bun_binary(&manifest_dir, &output_dir, &target_os, &target_arch);

    // Copy Agent binary
    copy_agent_binary(&manifest_dir, &output_dir, &target_os, &target_arch);

    // Watch for changes
    println!("cargo:rerun-if-changed={BUN_BIN_DIR}/");
    println!("cargo:rerun-if-changed={AGENT_BIN_DIR}/");

    // Watch for changes in the plugin.ts asset file
    let plugin_ts = manifest_dir.join("src/apx/assets/plugin.ts");
    println!("cargo:rerun-if-changed={}", plugin_ts.display());
}

fn copy_bun_binary(manifest_dir: &Path, output_dir: &Path, target_os: &str, target_arch: &str) {
    let bun_src_name = bun_binary_name(target_os, target_arch)
        .unwrap_or_else(|| panic!("Unsupported target for bun: {target_os}-{target_arch}"));
    let bun_source = manifest_dir.join(BUN_BIN_DIR).join(bun_src_name);
    if !bun_source.exists() {
        panic!("Missing Bun binary at {}", bun_source.display());
    }
    let bun_dest_name = if target_os == "windows" {
        "bun.exe"
    } else {
        "bun"
    };
    let bun_dest = output_dir.join(bun_dest_name);
    fs::copy(&bun_source, &bun_dest).expect("Failed to copy Bun binary");
    set_executable_permissions(&bun_dest);
    println!("cargo:rerun-if-changed={}", bun_source.display());
}

fn copy_agent_binary(manifest_dir: &Path, output_dir: &Path, target_os: &str, target_arch: &str) {
    let agent_src_name = match agent_binary_name(target_os, target_arch) {
        Some(name) => name,
        None => {
            // Agent binary not available for this platform - skip silently
            // This allows development on platforms where agent binary hasn't been cross-compiled yet
            println!(
                "cargo:warning=Agent binary not available for {target_os}-{target_arch}, skipping"
            );
            return;
        }
    };

    let agent_source = manifest_dir.join(AGENT_BIN_DIR).join(agent_src_name);
    if !agent_source.exists() {
        // Agent binary not yet built - skip with warning
        println!(
            "cargo:warning=Agent binary not found at {}, skipping",
            agent_source.display()
        );
        return;
    }

    let agent_dest_name = if target_os == "windows" {
        "apx-agent.exe"
    } else {
        "apx-agent"
    };
    let agent_dest = output_dir.join(agent_dest_name);
    fs::copy(&agent_source, &agent_dest).expect("Failed to copy Agent binary");
    set_executable_permissions(&agent_dest);
    println!("cargo:rerun-if-changed={}", agent_source.display());
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) {
    let mut perms = fs::metadata(path)
        .expect("Failed to read binary metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("Failed to set binary permissions");
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) {
    // No-op on Windows
}

fn bun_binary_name(target_os: &str, target_arch: &str) -> Option<&'static str> {
    match (target_os, target_arch) {
        ("macos", "aarch64") => Some("bun-darwin-aarch64"),
        ("macos", "x86_64") => Some("bun-darwin-x64"),
        ("linux", "aarch64") => Some("bun-linux-aarch64"),
        ("linux", "x86_64") => Some("bun-linux-x64"),
        ("windows", "x86_64") => Some("bun-windows-x64.exe"),
        _ => None,
    }
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
