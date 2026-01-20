use std::env;
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BIN_DIR: &str = ".bins";
const OUTPUT_DIR: &str = "src/apx/binaries";
const MODELS_DIR: &str = "assets/models";

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

    // Verify model files exist (they will be embedded via include_bytes!)
    verify_model_files(&manifest_dir);
}

fn verify_model_files(manifest_dir: &PathBuf) {
    let models_dir = manifest_dir.join(MODELS_DIR).join("all-MiniLM-L6-v2");

    // Required model files that will be embedded
    let required_files = [
        "tokenizer.json",
        "config.json",
        "model.safetensors",
    ];

    if !models_dir.exists() {
        println!("cargo:warning=Models directory not found at {}.", models_dir.display());
        println!("cargo:warning=Run 'python3 scripts/download_model.py' to download the embedding model.");
        println!("cargo:warning=The model will be embedded into the binary at compile time.");
        return;
    }

    let mut missing_files = Vec::new();
    for file in &required_files {
        let file_path = models_dir.join(file);
        if !file_path.exists() {
            missing_files.push(*file);
        }
    }

    if !missing_files.is_empty() {
        println!("cargo:warning=Missing required model files: {:?}", missing_files);
        println!("cargo:warning=Run 'python3 scripts/download_model.py' to download the embedding model.");
    } else {
        // Watch for changes in model files
        println!("cargo:rerun-if-changed={}", models_dir.display());
        for file in &required_files {
            println!("cargo:rerun-if-changed={}", models_dir.join(file).display());
        }
    }
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