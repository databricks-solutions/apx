use std::env;
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BUN_BIN_DIR: &str = ".bins/bun";
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
    let bun_src_name = bun_binary_name(&target_os, &target_arch)
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

    // Watch for changes
    println!("cargo:rerun-if-changed={}", bun_source.display());
    println!("cargo:rerun-if-changed={BUN_BIN_DIR}/");

    // Watch for changes in the plugin.ts asset file
    let plugin_ts = manifest_dir.join("src/apx/assets/plugin.ts");
    println!("cargo:rerun-if-changed={}", plugin_ts.display());
}

#[cfg(unix)]
fn set_executable_permissions(path: &PathBuf) {
    let mut perms = fs::metadata(path)
        .expect("Failed to read binary metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("Failed to set binary permissions");
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &PathBuf) {
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
