use clap::Args;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;

use crate::common::{format_elapsed_ms, read_project_metadata, spinner};
use crate::{bun_binary_path, cli::run_cli_async};

use super::cache::sync_registry_indexes;
use super::{
    AddPlan, PlannedFile, ResolvedComponent, UiConfig, apply_css_updates, collect_css_mutations,
    plan_add,
};
use crate::cli::components::utils::format_relative_path;

fn resolve_app_dir(app_path: Option<PathBuf>) -> PathBuf {
    app_path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn print_plan_summary(plan: &AddPlan) {
    println!("Components:");
    for component in &plan.components {
        let registry = component.registry.as_deref().unwrap_or("default");
        println!("  - {} ({})", component.name, registry);
    }

    println!("Files:");
    for file in &plan.files_to_write {
        println!(
            "  {} (from {})",
            file.relative_path.display(),
            file.source_component
        );
    }

    if !plan.component_deps.is_empty() {
        println!("Dependencies:");
        for dep in &plan.component_deps {
            println!("  - {dep}");
        }
    }

    if !plan.warnings.is_empty() {
        println!("Warnings:");
        for warning in &plan.warnings {
            let indented = warning.replace('\n', "\n    ");
            println!("  - {indented}");
        }
    }
}

enum WriteResult {
    Written,
    Unchanged,
}

fn write_file_if_changed(
    file: &PlannedFile,
    force: bool,
    app_dir: &Path,
) -> Result<WriteResult, String> {
    if file.absolute_path.exists() {
        let existing = std::fs::read_to_string(&file.absolute_path)
            .map_err(|e| format!("Failed to read {}: {e}", file.absolute_path.display()))?;
        if existing == file.content {
            return Ok(WriteResult::Unchanged);
        }
        if !force {
            return Err(format!(
                "File already exists (use --force): {}",
                format_relative_path(&file.absolute_path, app_dir)
            ));
        }
    }

    if let Some(parent) = file.absolute_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    std::fs::write(&file.absolute_path, &file.content)
        .map_err(|e| format!("Failed to write {}: {e}", file.absolute_path.display()))?;
    Ok(WriteResult::Written)
}

// ============================================================================
// API Layer - Used by both CLI and init
// ============================================================================

/// Input for adding a component via the API
#[derive(Debug, Clone)]
pub struct ComponentInput {
    pub name: String,
    pub registry: Option<String>,
}

impl ComponentInput {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            registry: None,
        }
    }

    pub fn with_registry(name: impl Into<String>, registry: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            registry: Some(registry.into()),
        }
    }
}

/// Result of adding components via the API
#[derive(Debug, Default)]
pub struct AddComponentsResult {
    pub written_paths: Vec<PathBuf>,
    pub unchanged_paths: Vec<PathBuf>,
    pub dependencies_installed: Vec<String>,
    pub css_updated_path: Option<String>,
    pub warnings: Vec<String>,
}

/// Add one or more components without console output.
///
/// This is the core API used by both the CLI `add` command and the `init` command.
/// It handles:
/// - Resolving all components and their dependencies
/// - Deduplicating files across components
/// - Writing files to disk
/// - Installing npm dependencies (once, batched)
/// - Updating CSS with required variables/rules
///
/// The caller is responsible for console output (spinners, success messages, etc.)
pub async fn add_components(
    app_dir: &Path,
    components: &[ComponentInput],
    force: bool,
) -> Result<AddComponentsResult, String> {
    if components.is_empty() {
        return Ok(AddComponentsResult::default());
    }

    // Load metadata and config
    let metadata = read_project_metadata(app_dir)?;
    let cfg = UiConfig::from_metadata(&metadata, app_dir);
    let client = reqwest::Client::new();

    // Collect all plans for all components
    let mut all_files: Vec<PlannedFile> = Vec::new();
    let mut all_deps: BTreeSet<String> = BTreeSet::new();
    let mut all_resolved: Vec<ResolvedComponent> = Vec::new();
    let mut all_warnings: Vec<String> = Vec::new();
    let mut seen_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for input in components {
        // Parse component name to extract registry prefix if present (e.g., @animate-ui/button)
        let (registry, component_name) = if input.name.starts_with('@') && input.registry.is_none()
        {
            if let Some((prefix, name)) = input.name.split_once('/') {
                (Some(prefix.to_string()), name.to_string())
            } else {
                (input.registry.clone(), input.name.clone())
            }
        } else {
            (input.registry.clone(), input.name.clone())
        };

        let plan = plan_add(&client, app_dir, &cfg, registry.as_deref(), &component_name).await?;

        // Deduplicate files across components
        for file in plan.files_to_write {
            if !seen_files.contains(&file.absolute_path) {
                seen_files.insert(file.absolute_path.clone());
                all_files.push(file);
            }
        }

        // Collect dependencies
        all_deps.extend(plan.component_deps);

        // Collect resolved components for CSS mutations
        all_resolved.extend(plan.components);

        // Collect warnings
        all_warnings.extend(plan.warnings);
    }

    let mut result = AddComponentsResult {
        warnings: all_warnings,
        ..Default::default()
    };

    // Write all files
    for file in &all_files {
        match write_file_if_changed(file, force, app_dir)? {
            WriteResult::Written => result.written_paths.push(file.absolute_path.clone()),
            WriteResult::Unchanged => result.unchanged_paths.push(file.absolute_path.clone()),
        }
    }

    // Install all dependencies at once
    if !all_deps.is_empty() {
        let deps: Vec<String> = all_deps.iter().cloned().collect();
        bun_add(app_dir, &deps).await?;
        result.dependencies_installed = deps;
    }

    // Apply CSS updates for all components at once
    let css_mutations = collect_css_mutations(&all_resolved);
    if !css_mutations.is_empty() {
        let css_path = cfg.css_path();
        match apply_css_updates(&css_path, css_mutations) {
            Ok(()) => {
                result.css_updated_path = Some(format_relative_path(&css_path, app_dir));
            }
            Err(e) => {
                result.warnings.push(format!(
                    "Failed to automatically update CSS: {e}. You may need to manually add CSS variables."
                ));
            }
        }
    }

    // Sync registry indexes silently
    let _ = sync_registry_indexes(app_dir, false).await;

    Ok(result)
}

#[derive(Args, Debug, Clone)]
pub struct ComponentsAddArgs {
    /// Component name (e.g. button, dialog)
    pub component: String,

    /// Registry name (from components.json)
    #[arg(long)]
    pub registry: Option<String>,

    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,

    /// Print actions without writing files
    #[arg(long)]
    pub dry_run: bool,

    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: ComponentsAddArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

/// CLI handler for the `add` command. Uses the API and handles console output.
pub async fn run_inner(args: ComponentsAddArgs) -> Result<(), String> {
    let start_time = Instant::now();
    let app_dir = resolve_app_dir(args.app_path.clone());

    // Handle dry-run separately since API doesn't support it
    if args.dry_run {
        return run_dry_run(&app_dir, &args.component, args.registry.as_deref()).await;
    }

    // Parse component name for display
    let display_name = if args.component.starts_with('@') && args.registry.is_none() {
        if let Some((_, name)) = args.component.split_once('/') {
            name.to_string()
        } else {
            args.component.clone()
        }
    } else {
        args.component.clone()
    };

    // Print component name in yellow
    println!("✨ Adding component \x1b[33m{display_name}\x1b[0m");

    // Use the API
    let input = match args.registry {
        Some(registry) => ComponentInput::with_registry(args.component, registry),
        None => ComponentInput::new(args.component),
    };

    let dep_spinner = spinner("📦 Installing dependencies...");
    let result = add_components(&app_dir, &[input], args.force).await?;
    dep_spinner.finish_and_clear();

    // Print dependencies installed
    if !result.dependencies_installed.is_empty() {
        println!("✅ Dependencies installed");
    }

    // Print summary
    println!();
    if !result.written_paths.is_empty() {
        println!("📄 Files added:");
        for path in &result.written_paths {
            println!("   • {}", format_relative_path(path, &app_dir));
        }
    }

    if let Some(css_path) = &result.css_updated_path {
        println!("🎨 CSS file updated: {css_path}");
    }

    for warning in &result.warnings {
        eprintln!("\n⚠️  WARNING: {warning}");
    }

    println!(
        "\n🎉 Component added in {}\n",
        format_elapsed_ms(start_time)
    );

    Ok(())
}

/// Dry-run handler - shows what would be done without making changes
async fn run_dry_run(
    app_dir: &Path,
    component: &str,
    registry: Option<&str>,
) -> Result<(), String> {
    let metadata = read_project_metadata(app_dir)?;
    let cfg = UiConfig::from_metadata(&metadata, app_dir);
    let client = reqwest::Client::new();

    // Parse component name to extract registry prefix
    let (resolved_registry, component_name) = if component.starts_with('@') && registry.is_none() {
        if let Some((prefix, name)) = component.split_once('/') {
            (Some(prefix), name)
        } else {
            (registry, component)
        }
    } else {
        (registry, component)
    };

    let plan = plan_add(&client, app_dir, &cfg, resolved_registry, component_name).await?;
    print_plan_summary(&plan);
    Ok(())
}

async fn bun_add(app_dir: &Path, deps: &[String]) -> Result<(), String> {
    if deps.is_empty() {
        return Ok(());
    }

    let bun_path = bun_binary_path()?;
    let output = Command::new(bun_path)
        .arg("add")
        .args(deps)
        .current_dir(app_dir)
        .output()
        .await
        .map_err(|e| format!("Failed to install dependencies: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Failed to install dependencies. Stdout: {} Stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}
