use clap::Args;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::common::resolve_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::{format_elapsed_ms, read_project_metadata, spinner};

use apx_core::components::cache::sync_registry_indexes;
use apx_core::components::utils::format_relative_path;
use apx_core::components::{AddPlan, UiConfig, plan_add};

// Re-export from core so init.rs and other CLI code can use these
pub use apx_core::components::add::{ComponentInput, add_components};

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
    run_cli_async_helper(|| run_inner(args)).await
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

    // Sync registry indexes silently
    let _ = sync_registry_indexes(app_dir, false).await;

    Ok(())
}
