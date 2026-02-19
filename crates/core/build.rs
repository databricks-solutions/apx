use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Could not find workspace root");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    copy_agent_binary(workspace_root, &out_dir);
    copy_skill_files(workspace_root);

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
        workspace_root.join(".bins/agent").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("skills/apx").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("hooks/hooks.json").display()
    );
}

/// Copy a platform-specific binary from `.bins/<subdir>/` to `OUT_DIR/<dest_name>`.
fn copy_platform_binary(
    workspace_root: &std::path::Path,
    out_dir: &std::path::Path,
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

/// Copy skill files and hooks from the repo root into the claude addon template
/// directory so they get embedded by rust-embed. This keeps `skills/apx/` as the
/// single source of truth while still bundling them into the binary.
fn copy_skill_files(workspace_root: &std::path::Path) {
    let claude_addon = workspace_root.join("src/apx/templates/addons/claude");

    let copies: &[(&str, &str)] = &[
        ("skills/apx/SKILL.md", ".claude/skills/apx/SKILL.md"),
        (
            "skills/apx/backend-patterns.md",
            ".claude/skills/apx/backend-patterns.md",
        ),
        (
            "skills/apx/frontend-patterns.md",
            ".claude/skills/apx/frontend-patterns.md",
        ),
        ("hooks/hooks.json", "hooks/hooks.json"),
    ];

    for (src_rel, dst_rel) in copies {
        let src = workspace_root.join(src_rel);
        let dst = claude_addon.join(dst_rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("Failed to create directory {}: {e}", parent.display()));
        }
        fs::copy(&src, &dst).unwrap_or_else(|e| {
            panic!("Failed to copy {} -> {}: {e}", src.display(), dst.display())
        });
    }
}

fn copy_agent_binary(workspace_root: &std::path::Path, out_dir: &std::path::Path) {
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
