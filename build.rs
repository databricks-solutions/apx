use std::env;
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BIN_DIR: &str = ".bins";
const OUTPUT_DIR: &str = "src/apx/binaries";
const MODELS_DIR: &str = "assets/models";
const MODELS_OUTPUT_DIR: &str = "src/search/models";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let bin_name = bun_binary_name(&target_os, &target_arch)
        .unwrap_or_else(|| panic!("Unsupported target: {target_os}-{target_arch}"));

    let source = manifest_dir.join(BIN_DIR).join(bin_name);
    if !source.exists() {
        panic!("Missing Bun binary at {}", source.display());
    }

    let output_dir = manifest_dir.join(OUTPUT_DIR);
    fs::create_dir_all(&output_dir).expect("Failed to create binaries output dir");

    for entry in fs::read_dir(&output_dir).expect("Failed to read binaries output dir") {
        let entry = entry.expect("Failed to read binaries output entry");
        let path = entry.path();
        if path.is_file() {
            fs::remove_file(&path).expect("Failed to remove old Bun binary");
        }
    }

    let dest_name = if target_os == "windows" { "bun.exe" } else { "bun" };
    let dest = output_dir.join(dest_name);
    fs::copy(&source, &dest).expect("Failed to copy Bun binary");

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&dest)
            .expect("Failed to read copied Bun binary metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms).expect("Failed to set Bun binary permissions");
    }

    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}/", BIN_DIR);

    // Watch for changes in the plugin.ts asset file
    let plugin_ts = manifest_dir.join("src/apx/assets/plugin.ts");
    println!("cargo:rerun-if-changed={}", plugin_ts.display());

    // Copy model files to output directory
    copy_model_files(&manifest_dir);
}

fn copy_model_files(manifest_dir: &PathBuf) {
    let models_source = manifest_dir.join(MODELS_DIR);
    let models_output = manifest_dir.join(MODELS_OUTPUT_DIR);

    // Create output directory
    if let Err(e) = fs::create_dir_all(&models_output) {
        eprintln!("Warning: Failed to create models output dir: {}", e);
        return;
    }

    // Check if models directory exists
    if !models_source.exists() {
        println!("cargo:warning=Models directory not found at {}. Run 'python3 scripts/download_model.py' to download models.", models_source.display());
        return;
    }

    // Recursively copy all model files
    match copy_dir_recursive(&models_source, &models_output) {
        Ok(_) => {
            println!("cargo:warning=Copied model files from {} to {}", models_source.display(), models_output.display());
            // Watch for changes in the models directory
            println!("cargo:rerun-if-changed={}", models_source.display());
        }
        Err(e) => {
            eprintln!("Warning: Failed to copy model files: {}", e);
        }
    }
}

fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);

        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path)?;
        } else {
            fs::copy(&path, &dst_path)?;
        }
    }

    Ok(())
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