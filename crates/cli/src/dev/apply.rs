use clap::{Args, ValueEnum};
use dialoguer::Confirm;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::{Path, PathBuf};
use tera::Context;

use crate::common::{Assistant, Layout, find_app_dir};
use crate::run_cli_async_helper;
use apx_core::interop::{get_template_content, list_template_files};

/// Available addons that can be applied
#[derive(ValueEnum, Clone, Debug, Copy)]
#[value(rename_all = "lower")]
pub enum Addon {
    // Assistant addons (from common::Assistant)
    /// Cursor AI assistant rules
    Cursor,
    /// VSCode AI assistant rules
    Vscode,
    /// Claude AI assistant rules
    Claude,
    /// Codex AI assistant rules
    Codex,

    // Backend addons
    /// Lakebase (Databricks Database) integration
    Lakebase,
    /// SQL Warehouse connection
    Sql,
    /// Serving Endpoint client
    #[value(name = "serving-endpoint")]
    ServingEndpoint,
    /// Genie Space client
    Genie,

    // Layout addons (from common::Layout)
    /// Sidebar layout addon
    Sidebar,
}

impl Addon {
    /// Get the directory name for this addon in the templates folder
    fn directory_name(&self) -> &str {
        match self {
            // Assistant addons
            Addon::Cursor => Assistant::Cursor.directory_name(),
            Addon::Vscode => Assistant::Vscode.directory_name(),
            Addon::Claude => Assistant::Claude.directory_name(),
            Addon::Codex => Assistant::Codex.directory_name(),
            // Backend addons
            Addon::Lakebase => "lakebase",
            Addon::Sql => "sql",
            Addon::ServingEndpoint => "serving-endpoint",
            Addon::Genie => "genie",
            // Layout addons
            Addon::Sidebar => Layout::Sidebar.directory_name().unwrap_or("sidebar"),
        }
    }

    /// Check if this addon is a backend addon (uses AST-based application)
    fn is_backend(&self) -> bool {
        matches!(
            self,
            Addon::Lakebase | Addon::Sql | Addon::ServingEndpoint | Addon::Genie
        )
    }

    /// Get the BackendAddonSpec for this addon, if it's a backend addon
    fn backend_spec(&self) -> Option<BackendAddonSpec> {
        match self {
            Addon::Lakebase => Some(BackendAddonSpec {
                name: "lakebase",
                template_dir: "lakebase",
            }),
            Addon::Sql => Some(BackendAddonSpec {
                name: "sql",
                template_dir: "sql",
            }),
            Addon::ServingEndpoint => Some(BackendAddonSpec {
                name: "serving-endpoint",
                template_dir: "serving-endpoint",
            }),
            Addon::Genie => Some(BackendAddonSpec {
                name: "genie",
                template_dir: "genie",
            }),
            _ => None,
        }
    }
}

/// Specification for a backend addon.
struct BackendAddonSpec {
    /// Name (for display)
    name: &'static str,
    /// Template directory under addons/
    template_dir: &'static str,
}

/// A Python source code edit to apply via AST.
enum PythonEdit {
    /// Add import to a file (relative to app's src/{app_slug}/)
    AddImport { file: String, statement: String },
    /// Add TypeAlias member to the Dependencies class in dependencies.py
    AddAlias { type_alias_code: String },
}

/// Inline metadata parsed from a `# /// apx` ... `# ///` TOML block.
#[derive(serde::Deserialize, Default)]
struct AddonMetadata {
    /// Import statements to add to `backend/core/__init__.py`
    #[serde(default)]
    exports: Vec<String>,
    /// Import statements to add to `backend/core/dependencies.py`
    #[serde(default)]
    imports: Vec<String>,
    /// Python package dependencies to add to pyproject.toml
    #[serde(default)]
    dependencies: Vec<String>,
    /// TypeAlias members to add to the `Dependencies` class
    #[serde(default)]
    aliases: Vec<String>,
}

/// Extract and parse the `# /// apx` TOML block from a Python source file.
fn parse_addon_metadata(source: &str) -> AddonMetadata {
    // Extract the TOML payload between `# /// apx` and `# ///`
    let mut in_block = false;
    let mut toml_lines = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed == "# /// apx" {
            in_block = true;
            continue;
        }
        if in_block {
            if trimmed == "# ///" {
                break;
            }
            // Strip the leading `# ` comment prefix
            let payload = trimmed
                .strip_prefix("# ")
                .unwrap_or(trimmed.strip_prefix("#").unwrap_or(trimmed));
            toml_lines.push(payload);
        }
    }

    if toml_lines.is_empty() {
        return AddonMetadata::default();
    }

    let toml_str = toml_lines.join("\n");
    match toml::from_str(&toml_str) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Failed to parse # /// apx metadata: {e}");
            AddonMetadata::default()
        }
    }
}

/// Convert an [`AddonMetadata`] into a list of [`PythonEdit`]s.
fn metadata_to_edits(meta: &AddonMetadata) -> Vec<PythonEdit> {
    let mut edits = Vec::new();
    for stmt in &meta.exports {
        edits.push(PythonEdit::AddImport {
            file: "backend/core/__init__.py".into(),
            statement: stmt.clone(),
        });
    }
    for stmt in &meta.imports {
        edits.push(PythonEdit::AddImport {
            file: "backend/core/dependencies.py".into(),
            statement: stmt.clone(),
        });
    }
    for code in &meta.aliases {
        edits.push(PythonEdit::AddAlias {
            type_alias_code: code.clone(),
        });
    }
    edits
}

#[derive(Args, Debug, Clone)]
pub struct ApplyArgs {
    /// The addon to apply
    #[arg(value_enum)]
    pub addon: Addon,

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

async fn run_inner(args: ApplyArgs) -> Result<(), String> {
    let addon = args.addon;
    let yes = args.yes;
    let app_dir = find_app_dir(args.app_path)?;

    // Read project context
    let (app_name, app_slug) = read_project_context(&app_dir)?;

    if addon.is_backend() {
        return apply_backend_addon(addon, yes, &app_dir, &app_slug);
    }

    let addon_prefix = format!("addons/{}/", addon.directory_name());
    let addon_files = list_template_files(&addon_prefix);

    if addon_files.is_empty() {
        return Err(format!(
            "Addon '{}' not found (no embedded templates with prefix '{}')",
            addon.directory_name(),
            addon_prefix,
        ));
    }

    println!(
        "Applying {} addon to {}...\n",
        addon.directory_name(),
        app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.clone())
            .display()
    );

    // Collect all file changes
    let changes =
        collect_file_changes(&addon_prefix, &addon_files, &app_dir, &app_name, &app_slug)?;

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
        addon.directory_name(),
        created,
        modified
    );

    Ok(())
}

/// Apply a backend addon using AST-based edits.
fn apply_backend_addon(
    addon: Addon,
    _yes: bool,
    app_dir: &Path,
    app_slug: &str,
) -> Result<(), String> {
    let spec = addon.backend_spec().ok_or("Not a backend addon")?;

    println!(
        "Applying {} backend addon to {}...\n",
        spec.name,
        app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf())
            .display()
    );

    let src_prefix = PathBuf::from("src").join(app_slug);

    // 1. Copy template files from addon (embedded)
    let addon_prefix = format!("addons/{}/", spec.template_dir);
    let addon_files = list_template_files(&addon_prefix);
    let mut copied_files = Vec::new();
    for file_path in &addon_files {
        let rel = file_path
            .strip_prefix(&addon_prefix)
            .unwrap_or(file_path.as_str());
        let mut path_str = rel.to_string();
        if path_str.contains("/base/") || path_str.starts_with("base/") {
            path_str = path_str
                .replace("/base/", &format!("/{app_slug}/"))
                .replace("base/", &format!("{app_slug}/"));
        }
        let is_template = path_str.ends_with(".jinja2");
        // Skip jinja2 config templates (databricks.yml, pyproject.toml, .env) — those need special handling
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

    // 2. Parse metadata from copied Python files and apply AST edits
    let mut ast_edits_applied = 0;
    let mut all_python_deps: Vec<String> = Vec::new();
    for file_rel in &copied_files {
        if file_rel.contains("/backend/core/") && file_rel.ends_with(".py") {
            let full_path = app_dir.join(file_rel);
            let source = fs::read_to_string(&full_path).map_err(|e| format!("Read error: {e}"))?;
            let meta = parse_addon_metadata(&source);
            all_python_deps.extend(meta.dependencies.iter().cloned());
            for edit in &metadata_to_edits(&meta) {
                match edit {
                    PythonEdit::AddImport { file, statement } => {
                        let target = app_dir.join(&src_prefix).join(file);
                        if !target.exists() {
                            tracing::warn!(
                                "Target file for AST edit not found: {}",
                                target.display()
                            );
                            continue;
                        }
                        let source =
                            fs::read_to_string(&target).map_err(|e| format!("Read error: {e}"))?;
                        match apx_core::py_edit::add_import(&source, statement) {
                            Ok(new_source) => {
                                fs::write(&target, new_source)
                                    .map_err(|e| format!("Write error: {e}"))?;
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
                    PythonEdit::AddAlias { type_alias_code } => {
                        let target = app_dir
                            .join(&src_prefix)
                            .join("backend/core/dependencies.py");
                        if !target.exists() {
                            tracing::warn!("dependencies.py not found: {}", target.display());
                            continue;
                        }
                        let source =
                            fs::read_to_string(&target).map_err(|e| format!("Read error: {e}"))?;
                        match apx_core::py_edit::add_class_member(
                            &source,
                            "Dependencies",
                            type_alias_code,
                        ) {
                            Ok(new_source) => {
                                fs::write(&target, new_source)
                                    .map_err(|e| format!("Write error: {e}"))?;
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
        }
    }

    // 3. Add Python dependencies to pyproject.toml (from metadata)
    if !all_python_deps.is_empty() {
        let pyproject_path = app_dir.join("pyproject.toml");
        crate::common::modify_pyproject(&pyproject_path, |doc| {
            let project = doc["project"]
                .as_table_mut()
                .ok_or("Missing [project] in pyproject.toml")?;
            let deps = project["dependencies"]
                .as_array_mut()
                .ok_or("Missing project.dependencies")?;
            for dep in &all_python_deps {
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

    if !copied_files.is_empty() {
        println!("\x1b[32mFiles copied:\x1b[0m");
        for f in &copied_files {
            println!("  \x1b[32m+\x1b[0m {f}");
        }
    }

    println!(
        "\n\x1b[32m✓\x1b[0m Applied {} backend addon: {} file(s) copied, {} AST edit(s) applied",
        spec.name,
        copied_files.len(),
        ast_edits_applied
    );

    Ok(())
}

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
