use clap::Args;
use dialoguer::Confirm;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tera::Context;

use crate::common::{find_app_dir, has_ui_config};
use crate::components::add::{ComponentInput, add_components};
use crate::init::merge_ui_pyproject_config;
use crate::run_cli_async_helper;
use apx_core::common::{BunCommand, format_elapsed_ms, spinner};
use apx_core::interop::{get_template_content, list_template_files};

// ─── Addon manifest types ───────────────────────────────

#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct AddonManifest {
    #[serde(default)]
    pub addon: AddonInfo,
    #[serde(default)]
    pub python: PythonMeta,
    #[serde(default)]
    pub typescript: TypeScriptMeta,
    #[serde(default)]
    pub components: ComponentsMeta,
    #[serde(default)]
    pub config: ConfigMeta,
}

#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct AddonInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub group_display_name: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub skill_path: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct PythonMeta {
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub edits: PythonEdits,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct PythonEdits {
    #[serde(default)]
    pub exports: Vec<String>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<AliasEntry>,
}

#[derive(serde::Deserialize)]
pub(crate) struct AliasEntry {
    pub code: String,
    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct TypeScriptMeta {
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct ComponentsMeta {
    #[serde(default)]
    pub install: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct ConfigMeta {
    #[serde(default)]
    pub requires_bun: bool,
}

/// Read and parse the `addon.toml` manifest for an addon.
pub(crate) fn read_addon_manifest(addon_dir_name: &str) -> Option<AddonManifest> {
    let path = format!("addons/{}/addon.toml", addon_dir_name);
    let content = get_template_content(&path).ok()?;
    toml::from_str(&content).ok()
}

/// Discover all available addons by scanning embedded template files for `addon.toml`.
/// Returns a list of (directory_name, manifest) pairs.
pub(crate) fn discover_all_addons() -> Vec<(String, AddonManifest)> {
    let all_files = list_template_files("addons/");
    let mut seen = std::collections::HashSet::new();
    let mut addons = Vec::new();

    for file in &all_files {
        // Match paths like "addons/<name>/addon.toml"
        if let Some(rest) = file.strip_prefix("addons/")
            && let Some(slash) = rest.find('/')
        {
            let dir_name = &rest[..slash];
            if seen.insert(dir_name.to_string())
                && let Some(manifest) = read_addon_manifest(dir_name)
            {
                addons.push((dir_name.to_string(), manifest));
            }
        }
    }

    addons
}

/// Validate an addon name against discovered addons.
/// Returns the addon name if valid, or an error listing available addons.
fn parse_addon_name(s: &str) -> Result<String, String> {
    let all = discover_all_addons();
    if all.iter().any(|(name, _)| name == s) {
        return Ok(s.to_string());
    }
    let mut lines = vec![format!("unknown addon '{s}'")];
    lines.push(String::new());
    lines.push("Available addons:".to_string());
    for (name, manifest) in &all {
        let desc = &manifest.addon.description;
        if desc.is_empty() {
            lines.push(format!("  {name}"));
        } else {
            lines.push(format!("  {name:12} {desc}"));
        }
    }
    Err(lines.join("\n"))
}

/// Check if an addon's manifest has non-empty Python edits.
fn has_python_edits(manifest: &AddonManifest) -> bool {
    !manifest.python.edits.exports.is_empty()
        || !manifest.python.edits.imports.is_empty()
        || !manifest.python.edits.aliases.is_empty()
}

/// Check whether a given addon is already applied by inspecting the project.
fn is_addon_applied(addon_name: &str, app_dir: &Path) -> Result<bool, String> {
    match addon_name {
        "ui" => {
            let pyproject_path = app_dir.join("pyproject.toml");
            Ok(has_ui_config(&pyproject_path))
        }
        _ => {
            // For other addons, we don't have a reliable check yet
            Ok(false)
        }
    }
}

// ─── Python edit types ──────────────────────────────────

/// A Python source code edit to apply via AST.
enum PythonEdit {
    /// Add import to a file (relative to app's src/{app_slug}/)
    AddImport { file: String, statement: String },
    /// Add TypeAlias member to the Dependencies class in dependencies.py
    AddAlias {
        type_alias_code: String,
        doc: Option<String>,
    },
}

/// Convert [`PythonEdits`] from manifest into a list of [`PythonEdit`]s.
fn metadata_to_edits(edits: &PythonEdits) -> Vec<PythonEdit> {
    let mut result = Vec::new();
    for stmt in &edits.exports {
        result.push(PythonEdit::AddImport {
            file: "backend/core/__init__.py".into(),
            statement: stmt.clone(),
        });
    }
    for stmt in &edits.imports {
        result.push(PythonEdit::AddImport {
            file: "backend/core/dependencies.py".into(),
            statement: stmt.clone(),
        });
    }
    for alias in &edits.aliases {
        result.push(PythonEdit::AddAlias {
            type_alias_code: alias.code.clone(),
            doc: alias.doc.clone(),
        });
    }
    result
}

// ─── CLI args ───────────────────────────────────────────

#[derive(Args, Debug, Clone)]
pub struct ApplyArgs {
    /// The addon to apply
    #[arg(value_parser = parse_addon_name)]
    pub addon: String,

    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,

    #[arg(
        long = "yes",
        short = 'y',
        help = "Skip confirmation prompt and apply changes automatically"
    )]
    pub yes: bool,
}

pub async fn run(args: ApplyArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

// ─── File change tracking ───────────────────────────────

/// Represents a file that will be created or modified
#[derive(Debug)]
struct FileChange {
    /// Relative path from app root
    rel_path: String,
    /// New content to write
    new_content: String,
    /// Existing content (None if file doesn't exist)
    existing_content: Option<String>,
}

impl FileChange {
    fn is_new(&self) -> bool {
        self.existing_content.is_none()
    }

    fn is_modified(&self) -> bool {
        match &self.existing_content {
            Some(existing) => existing != &self.new_content,
            None => false,
        }
    }

    /// Generate a unified diff for this file change
    fn generate_diff(&self) -> Option<String> {
        let existing = self.existing_content.as_ref()?;
        if existing == &self.new_content {
            return None;
        }

        let diff = TextDiff::from_lines(existing, &self.new_content);
        let mut output = String::new();

        output.push_str(&format!("\x1b[1m--- {} (current)\x1b[0m\n", self.rel_path));
        output.push_str(&format!("\x1b[1m+++ {} (new)\x1b[0m\n", self.rel_path));

        for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
            if idx > 0 {
                output.push_str("...\n");
            }
            for op in group {
                for change in diff.iter_changes(op) {
                    let (sign, color) = match change.tag() {
                        ChangeTag::Delete => ("-", "\x1b[31m"),
                        ChangeTag::Insert => ("+", "\x1b[32m"),
                        ChangeTag::Equal => (" ", ""),
                    };
                    output.push_str(color);
                    output.push_str(sign);
                    output.push_str(change.value());
                    if change.missing_newline() {
                        output.push('\n');
                    }
                    if !color.is_empty() {
                        output.push_str("\x1b[0m");
                    }
                }
            }
        }

        Some(output)
    }
}

// ─── Main apply flow ────────────────────────────────────

async fn run_inner(args: ApplyArgs) -> Result<(), String> {
    let yes = args.yes;
    let app_dir = find_app_dir(args.app_path)?;

    // Read project context
    let (app_name, app_slug) = read_project_context(&app_dir)?;

    // Apply addon (and its dependencies recursively)
    apply_single_addon(&args.addon, yes, &app_dir, &app_name, &app_slug).await
}

/// Apply a single addon by name, auto-resolving any `depends_on` first.
async fn apply_single_addon(
    addon_name: &str,
    yes: bool,
    app_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<(), String> {
    let manifest = read_addon_manifest(addon_name);

    // Auto-resolve dependencies: apply any missing depends_on addons first
    if let Some(ref manifest) = manifest {
        for dep in &manifest.addon.depends_on {
            if !is_addon_applied(dep, app_dir)? {
                println!(
                    "📦 Addon '{}' requires '{}' — applying it first...\n",
                    addon_name, dep
                );
                Box::pin(apply_single_addon(dep, yes, app_dir, app_name, app_slug)).await?;
                println!();
            }
        }

        // Resolve bun if needed
        if manifest.config.requires_bun {
            let _bun = BunCommand::new().await?;
        }
    }

    // Apply backend addon if manifest has python edits, otherwise apply file addon
    if let Some(ref manifest) = manifest
        && has_python_edits(manifest)
    {
        apply_backend_addon(addon_name, manifest, yes, app_dir, app_slug)?;
    } else {
        apply_file_addon_by_name(addon_name, yes, app_dir, app_name, app_slug)?;
    }

    // Handle UI addon's pyproject merge
    if addon_name == "ui" {
        merge_ui_pyproject_config(app_dir, app_slug)?;
    }

    // Install components from manifest
    if let Some(ref manifest) = manifest {
        let components: Vec<ComponentInput> = manifest
            .components
            .install
            .iter()
            .map(ComponentInput::new)
            .collect();
        if !components.is_empty() {
            let components_start = Instant::now();
            let sp = spinner("🎨 Adding components...");
            let result = add_components(app_dir, &components, true).await?;
            sp.finish_and_clear();
            println!(
                "✅ Components added ({})",
                format_elapsed_ms(components_start)
            );
            if !result.warnings.is_empty() {
                for warning in &result.warnings {
                    eprintln!("   ⚠️  {warning}");
                }
            }
        }
    }

    Ok(())
}

/// Apply a non-backend addon using file copy with diff preview.
fn apply_file_addon_by_name(
    addon_name: &str,
    yes: bool,
    app_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<(), String> {
    let addon_prefix = format!("addons/{}/", addon_name);
    let addon_files = list_template_files(&addon_prefix);

    if addon_files.is_empty() {
        return Err(format!(
            "Addon '{}' not found (no embedded templates with prefix '{}')",
            addon_name, addon_prefix,
        ));
    }

    println!(
        "Applying {} addon to {}...\n",
        addon_name,
        app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf())
            .display()
    );

    // Collect all file changes
    let changes = collect_file_changes(&addon_prefix, &addon_files, app_dir, app_name, app_slug)?;

    if changes.is_empty() {
        println!("No changes to apply.");
        return Ok(());
    }

    // Separate new files and modified files
    let new_files: Vec<_> = changes.iter().filter(|c| c.is_new()).collect();
    let modified_files: Vec<_> = changes.iter().filter(|c| c.is_modified()).collect();
    let unchanged_count = changes.len() - new_files.len() - modified_files.len();

    // Display summary
    if !new_files.is_empty() {
        println!("\x1b[32mFiles to be created:\x1b[0m");
        for file in &new_files {
            println!("  \x1b[32m+\x1b[0m {}", file.rel_path);
        }
        println!();
    }

    if !modified_files.is_empty() {
        println!("\x1b[33mFiles to be modified:\x1b[0m");
        for file in &modified_files {
            println!("  \x1b[33m~\x1b[0m {}", file.rel_path);
        }
        println!();

        // Show diffs for modified files
        println!("\x1b[1m--- Diffs ---\x1b[0m\n");
        for file in &modified_files {
            if let Some(diff) = file.generate_diff() {
                println!("{}", diff);
                println!();
            }
        }
    }

    if unchanged_count > 0 {
        println!("\x1b[90m{} file(s) unchanged\x1b[0m\n", unchanged_count);
    }

    // Summary line
    let total_changes = new_files.len() + modified_files.len();
    println!(
        "Summary: {} new, {} modified, {} unchanged",
        new_files.len(),
        modified_files.len(),
        unchanged_count
    );

    if total_changes == 0 {
        println!("All files are up to date.");
        return Ok(());
    }

    // Ask for confirmation unless -y flag is provided
    if !yes {
        let confirmed = Confirm::new()
            .with_prompt("Do you want to apply these changes?")
            .default(true)
            .interact()
            .map_err(|err| format!("Failed to read confirmation: {err}"))?;

        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Apply changes
    let mut created = 0;
    let mut modified = 0;

    for change in &changes {
        if change.is_new() || change.is_modified() {
            let target_path = app_dir.join(&change.rel_path);
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("Failed to create directory: {err}"))?;
            }
            fs::write(&target_path, &change.new_content)
                .map_err(|err| format!("Failed to write {}: {err}", change.rel_path))?;

            if change.is_new() {
                created += 1;
            } else {
                modified += 1;
            }
        }
    }

    println!(
        "\n\x1b[32m✓\x1b[0m Applied {} addon: {} file(s) created, {} file(s) modified",
        addon_name, created, modified
    );

    Ok(())
}

/// Apply Python AST edits (imports + class member aliases) and add dependencies from a manifest.
///
/// Called by both `init` (after `render_embedded_templates`) and `apply_backend_addon`.
/// Returns the number of AST edits applied.
pub(crate) fn apply_python_edits(
    manifest: &AddonManifest,
    app_dir: &Path,
    app_slug: &str,
) -> Result<usize, String> {
    let src_prefix = PathBuf::from("src").join(app_slug);
    let mut ast_edits_applied = 0;

    for edit in &metadata_to_edits(&manifest.python.edits) {
        match edit {
            PythonEdit::AddImport { file, statement } => {
                let target = app_dir.join(&src_prefix).join(file);
                if !target.exists() {
                    tracing::warn!("Target file for AST edit not found: {}", target.display());
                    continue;
                }
                let source = fs::read_to_string(&target).map_err(|e| format!("Read error: {e}"))?;
                match apx_core::py_edit::add_import(&source, statement) {
                    Ok(new_source) => {
                        fs::write(&target, new_source).map_err(|e| format!("Write error: {e}"))?;
                        ast_edits_applied += 1;
                    }
                    Err(apx_core::py_edit::PyEditError::AlreadyPresent(_)) => {
                        // Idempotent — skip
                    }
                    Err(e) => {
                        return Err(format!("AST edit error on {}: {e}", target.display()));
                    }
                }
            }
            PythonEdit::AddAlias {
                type_alias_code,
                doc,
            } => {
                let target = app_dir
                    .join(&src_prefix)
                    .join("backend/core/dependencies.py");
                if !target.exists() {
                    tracing::warn!("dependencies.py not found: {}", target.display());
                    continue;
                }
                let source = fs::read_to_string(&target).map_err(|e| format!("Read error: {e}"))?;
                let member_code = match doc {
                    Some(d) => {
                        let d = d.trim();
                        format!("{type_alias_code}\n\"\"\"{d}\"\"\"")
                    }
                    None => type_alias_code.clone(),
                };
                match apx_core::py_edit::add_class_member(&source, "Dependencies", &member_code) {
                    Ok(new_source) => {
                        fs::write(&target, new_source).map_err(|e| format!("Write error: {e}"))?;
                        ast_edits_applied += 1;
                    }
                    Err(apx_core::py_edit::PyEditError::AlreadyPresent(_)) => {}
                    Err(e) => {
                        return Err(format!("AST edit error on dependencies.py: {e}"));
                    }
                }
            }
        }
    }

    // Add Python dependencies to pyproject.toml
    if !manifest.python.dependencies.is_empty() {
        let pyproject_path = app_dir.join("pyproject.toml");
        crate::common::modify_pyproject(&pyproject_path, |doc| {
            let project = doc["project"]
                .as_table_mut()
                .ok_or("Missing [project] in pyproject.toml")?;
            let deps = project["dependencies"]
                .as_array_mut()
                .ok_or("Missing project.dependencies")?;
            for dep in &manifest.python.dependencies {
                let already = deps.iter().any(|v| {
                    v.as_str()
                        .map(|s| s.starts_with(dep.split('>').next().unwrap_or(dep)))
                        .unwrap_or(false)
                });
                if !already {
                    deps.push(dep.as_str());
                }
            }
            Ok(())
        })?;
    }

    Ok(ast_edits_applied)
}

/// Apply a backend addon using AST-based edits, reading metadata from addon.toml manifest.
fn apply_backend_addon(
    addon_dir: &str,
    manifest: &AddonManifest,
    _yes: bool,
    app_dir: &Path,
    app_slug: &str,
) -> Result<(), String> {
    println!(
        "Applying {} backend addon to {}...\n",
        addon_dir,
        app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf())
            .display()
    );

    // 1. Copy template files from addon (embedded)
    let addon_prefix = format!("addons/{}/", addon_dir);
    let addon_files = list_template_files(&addon_prefix);
    let mut copied_files = Vec::new();
    for file_path in &addon_files {
        let rel = file_path
            .strip_prefix(&addon_prefix)
            .unwrap_or(file_path.as_str());

        // Skip addon manifest — internal metadata, not user-facing
        if rel == "addon.toml" || rel.ends_with("/addon.toml") {
            continue;
        }

        let mut path_str = rel.to_string();
        if path_str.contains("/base/") || path_str.starts_with("base/") {
            path_str = path_str
                .replace("/base/", &format!("/{app_slug}/"))
                .replace("base/", &format!("{app_slug}/"));
        }
        let is_template = path_str.ends_with(".jinja2");
        // Skip jinja2 config templates (databricks.yml, .env) — those need special handling
        if is_template && !path_str.contains("/backend/") {
            // Render config templates
            let final_path = path_str.trim_end_matches(".jinja2");
            let target = app_dir.join(final_path);
            let content = get_template_content(file_path)?;
            let app_name_from_slug = app_slug.replace('_', "-");
            let mut ctx = Context::new();
            ctx.insert("app_name", &app_name_from_slug);
            ctx.insert("app_slug", app_slug);
            let rendered = tera::Tera::one_off(&content, &ctx, false)
                .map_err(|e| format!("Template render error: {e}"))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
            }
            fs::write(&target, rendered).map_err(|e| format!("write error: {e}"))?;
            copied_files.push(final_path.to_string());
            continue;
        }
        let final_path = if is_template {
            path_str.trim_end_matches(".jinja2").to_string()
        } else {
            path_str.clone()
        };
        let target = app_dir.join(&final_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        let content = get_template_content(file_path)?;
        fs::write(&target, content.as_bytes()).map_err(|e| format!("write error: {e}"))?;
        copied_files.push(final_path);
    }

    // 2. Apply AST edits from manifest + add Python dependencies
    let ast_edits_applied = apply_python_edits(manifest, app_dir, app_slug)?;

    if !copied_files.is_empty() {
        println!("\x1b[32mFiles copied:\x1b[0m");
        for f in &copied_files {
            println!("  \x1b[32m+\x1b[0m {f}");
        }
    }

    println!(
        "\n\x1b[32m✓\x1b[0m Applied {} backend addon: {} file(s) copied, {} AST edit(s) applied",
        addon_dir,
        copied_files.len(),
        ast_edits_applied
    );

    Ok(())
}

// ─── Helpers ────────────────────────────────────────────

/// Read project context (app_name and app_slug) from pyproject.toml
fn read_project_context(app_dir: &Path) -> Result<(String, String), String> {
    let pyproject_path = app_dir.join("pyproject.toml");

    if !pyproject_path.exists() {
        return Err(format!(
            "pyproject.toml not found at {}. Are you in an apx project directory?",
            pyproject_path.display()
        ));
    }

    let content = fs::read_to_string(&pyproject_path)
        .map_err(|err| format!("Failed to read pyproject.toml: {err}"))?;

    let doc: toml::Value = content
        .parse()
        .map_err(|err| format!("Failed to parse pyproject.toml: {err}"))?;

    let app_name = doc
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or("Could not find project.name in pyproject.toml")?
        .to_string();

    // Convert app_name to app_slug (replace - with _)
    let app_slug = app_name.replace('-', "_");

    Ok((app_name, app_slug))
}

/// Collect all file changes from embedded templates matching a prefix
fn collect_file_changes(
    prefix: &str,
    files: &[String],
    target_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<Vec<FileChange>, String> {
    let mut changes = Vec::new();

    for file_path in files {
        let rel = file_path.strip_prefix(prefix).unwrap_or(file_path.as_str());

        // Skip addon manifest — internal metadata, not user-facing
        if rel == "addon.toml" || rel.ends_with("/addon.toml") {
            continue;
        }

        let mut path_str = rel.to_string();

        // Replace "base" with app_slug in paths
        if path_str.contains("/base/") || path_str.starts_with("base/") {
            path_str = path_str
                .replace("/base/", &format!("/{app_slug}/"))
                .replace("base/", &format!("{app_slug}/"));
        }

        let is_template = path_str.ends_with(".jinja2");
        let final_rel_path = if is_template {
            path_str.trim_end_matches(".jinja2").to_string()
        } else {
            path_str
        };

        let target_path = target_dir.join(&final_rel_path);

        let template_content = get_template_content(file_path)?;

        // Generate new content
        let new_content = if is_template {
            let mut context = Context::new();
            context.insert("app_name", app_name);
            context.insert("app_slug", app_slug);
            context.insert(
                "app_letter",
                &app_name.chars().next().unwrap_or('A').to_string(),
            );

            tera::Tera::one_off(&template_content, &context, false)
                .map_err(|err| format!("Failed to render template {file_path}: {err}"))?
        } else {
            template_content
        };

        // Read existing content if file exists
        let existing_content = if target_path.exists() {
            Some(
                fs::read_to_string(&target_path)
                    .map_err(|err| format!("Failed to read existing file: {err}"))?,
            )
        } else {
            None
        };

        changes.push(FileChange {
            rel_path: final_rel_path,
            new_content,
            existing_content,
        });
    }

    // Sort by path for consistent output
    changes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    Ok(changes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Set up a temp project dir with the base `dependencies.py`, mimicking what
    /// `render_embedded_templates("base/", ...)` produces.
    fn setup_base_project(app_slug: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let app_dir = dir.path().to_path_buf();

        let deps_dir = app_dir.join("src").join(app_slug).join("backend/core");
        fs::create_dir_all(&deps_dir).unwrap();

        // Write the base dependencies.py (matches the base template)
        let deps_py = deps_dir.join("dependencies.py");
        fs::write(
            &deps_py,
            r#"from __future__ import annotations

from typing import TypeAlias
from ._defaults import ConfigDependency, ClientDependency, UserWorkspaceClientDependency
from ._headers import HeadersDependency


class Dependencies:
    """FastAPI dependency injection shorthand for route handler parameters."""

    Client: TypeAlias = ClientDependency
    UserClient: TypeAlias = UserWorkspaceClientDependency
    Config: TypeAlias = ConfigDependency
    Headers: TypeAlias = HeadersDependency
"#,
        )
        .unwrap();

        // Write a minimal pyproject.toml so apply_python_edits can add deps if needed
        fs::write(
            app_dir.join("pyproject.toml"),
            "[project]\nname = \"test-app\"\ndependencies = []\n",
        )
        .unwrap();

        (dir, app_dir)
    }

    #[test]
    fn test_apply_python_edits_adds_sql_dependency() {
        let (_dir, app_dir) = setup_base_project("test_app");

        let manifest = read_addon_manifest("sql").expect("sql addon manifest must exist");
        let edits = apply_python_edits(&manifest, &app_dir, "test_app").unwrap();
        assert!(edits > 0, "should have applied at least one AST edit");

        // Re-read the file and verify via ruff parser that the alias is present
        let deps_path = app_dir.join("src/test_app/backend/core/dependencies.py");
        let source = fs::read_to_string(&deps_path).unwrap();

        // Trying to add the same alias again must return AlreadyPresent
        let err = apx_core::py_edit::add_class_member(
            &source,
            "Dependencies",
            "Sql: TypeAlias = SqlDependency",
        )
        .unwrap_err();
        assert!(
            matches!(err, apx_core::py_edit::PyEditError::AlreadyPresent(_)),
            "Dependencies.Sql should already be present, got: {err}"
        );

        // Verify the docstring was inserted below the alias
        assert!(
            source.contains("\"\"\"SQL Warehouse query dependency."),
            "docstring should be present in dependencies.py, got:\n{source}"
        );

        // Also verify the import was added
        let import_err =
            apx_core::py_edit::add_import(&source, "from .sql import SqlDependency").unwrap_err();
        assert!(
            matches!(
                import_err,
                apx_core::py_edit::PyEditError::AlreadyPresent(_)
            ),
            "SqlDependency import should already be present, got: {import_err}"
        );
    }

    #[test]
    fn test_apply_python_edits_idempotent() {
        let (_dir, app_dir) = setup_base_project("test_app");

        let manifest = read_addon_manifest("sql").expect("sql addon manifest must exist");

        // Apply twice
        let edits1 = apply_python_edits(&manifest, &app_dir, "test_app").unwrap();
        let edits2 = apply_python_edits(&manifest, &app_dir, "test_app").unwrap();

        assert!(edits1 > 0);
        assert_eq!(edits2, 0, "second apply should be a no-op");
    }
}
