use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Could not find workspace root");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    pack_templates(workspace_root, &out_dir);
    copy_bun_binary(workspace_root, &out_dir);

    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("src/apx/templates").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("src/apx/assets").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join(".bins/bun").display()
    );
}

/// Walk `src/apx/templates/` and pack all files into a simple binary archive.
///
/// Archive format:
/// ```text
/// [u32 LE: entry count]
/// For each entry:
///   [u32 LE: relative path length]
///   [N bytes: UTF-8 path string]
///   [u32 LE: content length]
///   [N bytes: file content]
/// ```
fn pack_templates(workspace_root: &Path, out_dir: &Path) {
    let templates_dir = workspace_root.join("src/apx/templates");
    assert!(
        templates_dir.is_dir(),
        "Templates directory not found: {}",
        templates_dir.display()
    );

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();

    collect_files(&templates_dir, &templates_dir, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut archive = Vec::new();
    // Entry count
    archive.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (rel_path, content) in &entries {
        let path_bytes = rel_path.as_bytes();
        archive.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        archive.extend_from_slice(path_bytes);
        archive.extend_from_slice(&(content.len() as u32).to_le_bytes());
        archive.extend_from_slice(content);
    }

    let dest = out_dir.join("templates.pack");
    fs::write(&dest, &archive).expect("Failed to write templates.pack");

    println!(
        "cargo:warning=Packed {} template files into templates.pack",
        entries.len()
    );
}

fn collect_files(base: &Path, dir: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
    let read_dir =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("Failed to read {}: {e}", dir.display()));
    for entry in read_dir {
        let entry = entry.expect("Failed to read dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("Failed to get file type");

        if file_type.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == "__pycache__" {
                continue;
            }
            collect_files(base, &path, entries);
        } else if file_type.is_file() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".pyc") {
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .expect("Failed to strip prefix")
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read(&path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
            entries.push((rel, content));
        }
    }
}

fn copy_bun_binary(workspace_root: &Path, out_dir: &Path) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let bun_src_name = bun_binary_name(&target_os, &target_arch)
        .unwrap_or_else(|| panic!("Unsupported target for bun: {target_os}-{target_arch}"));

    let bun_source = workspace_root.join(".bins/bun").join(bun_src_name);
    assert!(
        bun_source.exists(),
        "Missing Bun binary at {}",
        bun_source.display()
    );

    let bun_dest = out_dir.join("bun");
    fs::copy(&bun_source, &bun_dest).expect("Failed to copy Bun binary");
    println!("cargo:rerun-if-changed={}", bun_source.display());
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
