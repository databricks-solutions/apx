use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::common::{BunCommand, read_project_metadata};

use super::cache::sync_registry_indexes;
use super::{
    PlannedFile, ResolvedComponent, UiConfig, apply_css_updates, collect_css_mutations, plan_add,
};
use crate::components::utils::format_relative_path;

#[derive(Debug)]
pub enum WriteResult {
    Written,
    Unchanged,
}

pub fn write_file_if_changed(
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
    let cfg = UiConfig::from_metadata(&metadata, app_dir)?;
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

pub async fn bun_add(app_dir: &Path, deps: &[String]) -> Result<(), String> {
    if deps.is_empty() {
        return Ok(());
    }

    let bun = BunCommand::new().await?;
    let output = bun
        .tokio_command()
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
