use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Could not find workspace root");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    pack_assets(workspace_root, &out_dir);
    copy_bun_binary(workspace_root, &out_dir);
    copy_agent_binary(workspace_root, &out_dir);

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
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join(".bins/agent").display()
    );
}

/// Pack templates and entrypoint.ts into a single zip archive (`assets.zip`).
///
/// Layout inside the zip:
/// - `templates/<relative path>` — all template files
/// - `entrypoint.ts` — the frontend entrypoint
fn pack_assets(workspace_root: &Path, out_dir: &Path) {
    let templates_dir = workspace_root.join("src/apx/templates");
    assert!(
        templates_dir.is_dir(),
        "Templates directory not found: {}",
        templates_dir.display()
    );

    let entrypoint_path = workspace_root.join("src/apx/assets/entrypoint.ts");
    assert!(
        entrypoint_path.is_file(),
        "entrypoint.ts not found: {}",
        entrypoint_path.display()
    );

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(&templates_dir, &templates_dir, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let dest = out_dir.join("assets.zip");
    let file = fs::File::create(&dest).expect("Failed to create assets.zip");
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // Add templates under templates/ prefix
    for (rel_path, content) in &entries {
        let archive_path = format!("templates/{rel_path}");
        zip.start_file(&archive_path, options)
            .unwrap_or_else(|e| panic!("Failed to add {archive_path} to zip: {e}"));
        zip.write_all(content)
            .unwrap_or_else(|e| panic!("Failed to write {archive_path}: {e}"));
    }

    // Add entrypoint.ts at the archive root
    let entrypoint_content = fs::read(&entrypoint_path).expect("Failed to read entrypoint.ts");
    zip.start_file("entrypoint.ts", options)
        .expect("Failed to add entrypoint.ts to zip");
    zip.write_all(&entrypoint_content)
        .expect("Failed to write entrypoint.ts");

    zip.finish().expect("Failed to finalize assets.zip");

    println!(
        "cargo:warning=Packed {} template files + entrypoint.ts into assets.zip",
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

/// Copy a platform-specific binary from `.bins/<subdir>/` to `OUT_DIR/<dest_name>`.
fn copy_platform_binary(
    workspace_root: &Path,
    out_dir: &Path,
    subdir: &str,
    dest_name: &str,
    platform_filename: impl FnOnce(&str, &str) -> Option<&'static str>,
) {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let src_name = platform_filename(&target_os, &target_arch)
        .unwrap_or_else(|| panic!("Unsupported target for {dest_name}: {target_os}-{target_arch}"));

    let source = workspace_root.join(".bins").join(subdir).join(src_name);
    assert!(
        source.exists(),
        "Missing {dest_name} binary at {}",
        source.display()
    );

    let dest = out_dir.join(dest_name);
    fs::copy(&source, &dest).unwrap_or_else(|e| panic!("Failed to copy {dest_name} binary: {e}"));
    println!("cargo:rerun-if-changed={}", source.display());
}

fn copy_bun_binary(workspace_root: &Path, out_dir: &Path) {
    copy_platform_binary(workspace_root, out_dir, "bun", "bun", |os, arch| {
        match (os, arch) {
            ("macos", "aarch64") => Some("bun-darwin-aarch64"),
            ("macos", "x86_64") => Some("bun-darwin-x64"),
            ("linux", "aarch64") => Some("bun-linux-aarch64"),
            ("linux", "x86_64") => Some("bun-linux-x64"),
            ("windows", "x86_64") => Some("bun-windows-x64.exe"),
            _ => None,
        }
    });
}

fn copy_agent_binary(workspace_root: &Path, out_dir: &Path) {
    // Naming convention from scripts/build_agent.py Target.output_filename
    copy_platform_binary(
        workspace_root,
        out_dir,
        "agent",
        "apx-agent",
        |os, arch| match (os, arch) {
            ("macos", "aarch64") => Some("apx-agent-darwin-aarch64"),
            ("macos", "x86_64") => Some("apx-agent-darwin-x64"),
            ("linux", "aarch64") => Some("apx-agent-linux-aarch64"),
            ("linux", "x86_64") => Some("apx-agent-linux-x64"),
            ("windows", "x86_64") => Some("apx-agent-windows-x64.exe"),
            _ => None,
        },
    );
}
