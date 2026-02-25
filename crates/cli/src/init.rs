use clap::Args;
use console::style;
use dialoguer::theme::{ColorfulTheme, Theme};
use dialoguer::{Confirm, Input, MultiSelect, Select};
use rand::seq::SliceRandom;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use tera::Context;
use tracing::debug;

/// (name, display_name, description, is_default, order)
type AddonEntry = (String, String, String, bool, i32);

/// Marker prefix for group header items in the multi-select list.
const HEADER_MARKER: &str = "\x01";

/// Custom theme that renders group headers as bold labels without checkboxes.
struct GroupedTheme {
    inner: ColorfulTheme,
}

impl GroupedTheme {
    fn new() -> Self {
        Self {
            inner: ColorfulTheme::default(),
        }
    }
}

impl Theme for GroupedTheme {
    fn format_multi_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        self.inner.format_multi_select_prompt(f, prompt)
    }

    fn format_multi_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        checked: bool,
        active: bool,
    ) -> fmt::Result {
        if let Some(label) = text.strip_prefix(HEADER_MARKER) {
            write!(f, "  {}:", style(label).for_stderr().bold())
        } else {
            self.inner
                .format_multi_select_prompt_item(f, text, checked, active)
        }
    }

    fn format_multi_select_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        selections: &[&str],
    ) -> fmt::Result {
        let filtered: Vec<&str> = selections
            .iter()
            .filter(|s| !s.starts_with(HEADER_MARKER))
            .copied()
            .collect();
        self.inner
            .format_multi_select_prompt_selection(f, prompt, &filtered)
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

use crate::common::{has_apx_config, modify_pyproject, resolve_app_dir};
use crate::components::add::{ComponentInput, add_components};
use crate::dev::apply::{apply_python_edits, discover_all_addons, read_addon_manifest};
use crate::run_cli_async_helper;
use apx_core::common::list_profiles;
use apx_core::common::{format_elapsed_ms, run_with_spinner, run_with_spinner_async, spinner};
use apx_core::dotenv::DotenvFile;
use apx_core::external::bun::Bun;
use apx_core::external::git::Git;
use apx_core::interop::{get_template_content, list_template_files};
use std::time::Instant;

/// Arguments for the `apx init` command.
#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// Optional path where the app will be created.
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    /// Project name (prompted interactively if omitted).
    #[arg(
        long = "name",
        short = 'n',
        help = "The name of the project. Will prompt if not provided"
    )]
    pub app_name: Option<String>,
    /// Addons to enable (comma-separated, e.g. --addons=ui,sidebar).
    /// Use --addons=none or --no-addons for a backend-only project.
    #[arg(
        long,
        value_delimiter = ',',
        help = "Addons to enable (comma-separated). Use 'none' or --no-addons for backend-only"
    )]
    pub addons: Option<Vec<String>>,
    /// Shorthand for --addons=none (backend-only, no addons)
    #[arg(long = "no-addons", conflicts_with = "addons")]
    pub no_addons: bool,
    /// Databricks CLI profile name (prompted interactively if omitted).
    #[arg(
        long,
        short = 'p',
        help = "The Databricks profile to use. Will prompt if not provided"
    )]
    pub profile: Option<String>,
    /// Initialize as a uv workspace member at the given relative path.
    #[arg(
        long = "as-member",
        value_name = "MEMBER_PATH",
        num_args = 0..=1,
        default_missing_value = "packages/app",
        help = "Initialize as a uv workspace member. Defaults to packages/app"
    )]
    pub as_member: Option<PathBuf>,
}

/// Execute the `apx init` command.
pub async fn run(args: InitArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(mut args: InitArgs) -> Result<(), String> {
    // Eagerly resolve uv (always needed)
    let _uv = apx_core::external::Uv::new().await?;

    let (workspace_root, app_path, is_member) = resolve_app_path(&mut args)?;

    println!("Welcome to apx 🚀\n");

    let (app_name, app_slug) = resolve_app_name(&mut args)?;

    let all_addons = discover_all_addons();
    let addon_names: Vec<String> = all_addons.iter().map(|(name, _)| name.clone()).collect();
    let selected_addons = select_addons(&args, &addon_names, &all_addons)?;

    let ui_enabled = selected_addons.iter().any(|a| a == "ui");

    select_profile(&mut args)?;

    // Resolve bun only for UI-enabled projects
    if ui_enabled {
        let _bun = Bun::new().await?;
    }

    println!(
        "\nInitializing app {} in {}\n",
        app_name,
        app_path
            .canonicalize()
            .unwrap_or_else(|_| app_path.clone())
            .display()
    );

    scaffold_project(
        &app_path,
        &app_name,
        &app_slug,
        &selected_addons,
        args.profile.as_deref(),
    )?;

    init_git_repo(&workspace_root, &app_path, is_member).await;

    install_addon_components(&app_path, &selected_addons).await?;

    print_success(&app_name, &workspace_root, &app_path, is_member);

    Ok(())
}

/// Resolve the workspace root, app path, and whether we are in member mode.
fn resolve_app_path(args: &mut InitArgs) -> Result<(PathBuf, PathBuf, bool), String> {
    let workspace_root = resolve_app_dir(args.app_path.take());

    // Auto-detect member mode: if CWD has pyproject.toml without [tool.apx],
    // the user is inside an existing project and we should init as a member.
    let pyproject_at_root = workspace_root.join("pyproject.toml");
    if args.as_member.is_none() && pyproject_at_root.exists() && !has_apx_config(&pyproject_at_root)
    {
        debug!("Existing pyproject.toml without [tool.apx] detected, switching to member mode");
        args.as_member = Some(PathBuf::from("packages/app"));
    }

    let is_member = args.as_member.is_some();

    // Resolve final app_path: for member mode, it's workspace_root/member_path
    let app_path = if let Some(ref member_path) = args.as_member {
        let full = workspace_root.join(member_path);
        debug!(
            "Member mode: workspace_root={}, app_path={}",
            workspace_root.display(),
            full.display()
        );
        ensure_workspace_config(&pyproject_at_root, member_path)?;
        full
    } else {
        workspace_root.clone()
    };

    Ok((workspace_root, app_path, is_member))
}

/// Prompt for or normalize the app name, returning `(app_name, app_slug)`.
fn resolve_app_name(args: &mut InitArgs) -> Result<(String, String), String> {
    if args.app_name.is_none() {
        let default_name = random_name();
        let name = Input::<String>::new()
            .with_prompt("What's the name of your app?")
            .default(default_name)
            .interact_text()
            .map_err(|err| format!("Failed to read app name: {err}"))?;
        args.app_name = Some(name);
    }

    let app_name_raw = args.app_name.take().unwrap_or_default();
    let app_name = normalize_app_name(&app_name_raw)?;
    let app_slug = app_name.replace('-', "_");
    Ok((app_name, app_slug))
}

/// Discover, validate, and interactively select addons to enable.
fn select_addons(
    args: &InitArgs,
    addon_names: &[String],
    all_addons: &[(String, crate::dev::apply::AddonManifest)],
) -> Result<Vec<String>, String> {
    if args.no_addons {
        return Ok(Vec::new());
    }

    if let Some(ref addons) = args.addons {
        if addons.len() == 1 && addons[0] == "none" {
            return Ok(Vec::new());
        }
        // Validate addon names
        for a in addons {
            if !addon_names.contains(a) {
                return Err(format!(
                    "Unknown addon '{}'. Available addons: {}",
                    a,
                    addon_names.join(", ")
                ));
            }
        }
        return Ok(addons.clone());
    }

    // Interactive grouped multi-select
    // Group addons by their group field, ordered: ui, backend, assistants, then rest
    let group_order = ["ui", "backend", "assistants"];
    let mut groups: BTreeMap<String, Vec<AddonEntry>> = BTreeMap::new();
    let mut group_display_names: BTreeMap<String, String> = BTreeMap::new();
    for (name, manifest) in all_addons {
        let group = if manifest.addon.group.is_empty() {
            "common".to_string()
        } else {
            manifest.addon.group.clone()
        };
        if !manifest.addon.group_display_name.is_empty() {
            group_display_names
                .entry(group.clone())
                .or_insert_with(|| manifest.addon.group_display_name.clone());
        }
        groups.entry(group).or_default().push((
            name.clone(),
            manifest.addon.display_name.clone(),
            manifest.addon.description.clone(),
            manifest.addon.default,
            manifest.addon.order,
        ));
    }
    // Sort addons within each group by order
    for group_addons in groups.values_mut() {
        group_addons.sort_by_key(|(_, _, _, _, order)| *order);
    }

    let mut ordered_groups: Vec<String> = Vec::new();
    for g in &group_order {
        if groups.contains_key(*g) {
            ordered_groups.push(g.to_string());
        }
    }
    for g in groups.keys() {
        if !ordered_groups.contains(g) {
            ordered_groups.push(g.clone());
        }
    }

    // Build flat list with header items interleaved
    let mut labels: Vec<String> = Vec::new();
    let mut defaults: Vec<bool> = Vec::new();
    let mut is_header: Vec<bool> = Vec::new();
    let mut addon_for_index: Vec<Option<String>> = Vec::new();

    for group_name in &ordered_groups {
        if let Some(group_addons) = groups.get(group_name) {
            // Group header (non-selectable visually)
            let header_label = group_display_names
                .get(group_name)
                .cloned()
                .unwrap_or_else(|| capitalize_first(group_name));
            labels.push(format!("{HEADER_MARKER}{header_label}"));
            defaults.push(false);
            is_header.push(true);
            addon_for_index.push(None);

            for (name, display_name, desc, default, _order) in group_addons {
                let label_name = if display_name.is_empty() {
                    name.as_str()
                } else {
                    display_name.as_str()
                };
                let label = if desc.is_empty() {
                    label_name.to_string()
                } else {
                    format!("{label_name} — {desc}")
                };
                labels.push(label);
                defaults.push(*default);
                is_header.push(false);
                addon_for_index.push(Some(name.clone()));
            }
        }
    }

    let label_refs: Vec<&str> = labels.iter().map(|l| l.as_str()).collect();
    let theme = GroupedTheme::new();
    let selections = MultiSelect::with_theme(&theme)
        .with_prompt(
            "Which addons would you like to enable? (space = toggle, enter = confirm, a = all)",
        )
        .items(&label_refs)
        .defaults(&defaults)
        .report(false)
        .interact()
        .map_err(|err| format!("Failed to select addons: {err}"))?;

    // Filter out header indices and map to addon names
    let selected: Vec<String> = selections
        .into_iter()
        .filter(|&i| !is_header[i])
        .filter_map(|i| addon_for_index[i].clone())
        .collect();

    if selected.is_empty() {
        println!("  Addons: none");
    } else {
        println!("  Addons: {}", selected.join(", "));
    }

    Ok(selected)
}

/// Prompt the user to select a Databricks CLI profile.
fn select_profile(args: &mut InitArgs) -> Result<(), String> {
    if args.profile.is_some() {
        return Ok(());
    }

    let available_profiles = list_profiles()?;
    if available_profiles.is_empty() {
        println!("No Databricks profiles found in ~/.databrickscfg");
        let should_prompt = Confirm::new()
            .with_prompt("Would you like to specify a profile name?")
            .default(false)
            .interact()
            .map_err(|err| format!("Failed to read profile choice: {err}"))?;
        if should_prompt {
            let profile = Input::<String>::new()
                .with_prompt("Enter profile name")
                .interact_text()
                .map_err(|err| format!("Failed to read profile: {err}"))?;
            args.profile = Some(profile);
        } else {
            args.profile = None;
        }
    } else {
        let mut items: Vec<String> = available_profiles.clone();
        items.push("Enter manually".into());
        items.push("Skip".into());

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Which Databricks profile would you like to use?")
            .items(&items)
            .default(0)
            .interact()
            .map_err(|err| format!("Failed to read profile: {err}"))?;

        if selection == items.len() - 1 {
            // "Skip"
            args.profile = None;
        } else if selection == items.len() - 2 {
            // "Enter manually"
            let profile = Input::<String>::new()
                .with_prompt("Enter profile name")
                .interact_text()
                .map_err(|err| format!("Failed to read profile: {err}"))?;
            args.profile = Some(profile);
        } else {
            args.profile = Some(available_profiles[selection].clone());
        }
    }

    Ok(())
}

/// Create directories, render templates, apply addons, and set the profile.
fn scaffold_project(
    app_path: &Path,
    app_name: &str,
    app_slug: &str,
    selected_addons: &[String],
    profile: Option<&str>,
) -> Result<(), String> {
    run_with_spinner(
        "📁 Preparing project layout...",
        "✅ Project layout prepared",
        || {
            ensure_dir(app_path)?;
            render_embedded_templates("base/", app_path, app_name, app_slug)?;

            let dist_dir = app_path.join("src").join(app_slug).join("__dist__");
            ensure_dir(&dist_dir)?;
            fs::write(dist_dir.join(".gitignore"), "*\n")
                .map_err(|err| format!("Failed to write dist .gitignore: {err}"))?;

            let build_dir = app_path.join(".build");
            ensure_dir(&build_dir)?;
            fs::write(build_dir.join(".gitignore"), "*\n")
                .map_err(|err| format!("Failed to write .build .gitignore: {err}"))?;

            // Apply all selected addon files
            for addon_name in selected_addons {
                let prefix = format!("addons/{addon_name}/");
                render_embedded_templates(&prefix, app_path, app_name, app_slug)?;

                // Apply Python AST edits and install skills from manifest
                if let Some(manifest) = read_addon_manifest(addon_name) {
                    if let Some(ref skill_path) = manifest.addon.skill_path {
                        crate::skill::install::install_skills_to(app_path, skill_path)?;
                    }
                    apply_python_edits(&manifest, app_path, app_slug)?;
                }

                // Handle UI addon's pyproject merge
                if addon_name == "ui" {
                    merge_ui_pyproject_config(app_path, app_slug)?;
                }
            }

            // Set profile AFTER addon templates, since addons may overwrite .env
            if let Some(profile) = profile {
                let mut dotenv = DotenvFile::read(&app_path.join(".env"))?;
                dotenv.update("DATABRICKS_CONFIG_PROFILE", profile)?;
            }

            Ok(())
        },
    )
}

/// Initialize a git repository at the workspace root (or app path if not a member).
async fn init_git_repo(workspace_root: &Path, app_path: &Path, is_member: bool) {
    let git_dir = if is_member { workspace_root } else { app_path };
    if Git::is_available().await {
        let git = match Git::new().map_err(|e| e.to_string()) {
            Ok(g) => g,
            Err(err) => {
                println!("⚠️  Git initialization failed: {err}");
                println!("   Continuing with project setup...");
                return;
            }
        };
        let inside =
            git.is_inside_work_tree(git_dir).await.unwrap_or(false) || has_git_dir(git_dir);
        if inside {
            println!("✓ Already in a git repository - skipping git initialization");
        } else {
            let git_result = run_with_spinner_async(
                "🔧 Initializing git repository...",
                "✅ Git repository initialized",
                || async {
                    git.init(git_dir)
                        .await
                        .map_err(|e| format!("Failed to initialize git repository: {e}"))?;
                    git.add(git_dir, &["."])
                        .await
                        .map_err(|e| format!("Failed to add files to git repository: {e}"))?;
                    git.commit(git_dir, "init")
                        .await
                        .map_err(|e| format!("Failed to commit files to git repository: {e}"))?;
                    Ok(())
                },
            )
            .await;

            if let Err(err) = git_result {
                println!("⚠️  Git initialization failed: {err}");
                println!("   Continuing with project setup...");
            }
        }
    } else {
        println!("⚠️  Git is not available - skipping git initialization");
    }
}

/// Install components declared in addon manifests.
async fn install_addon_components(
    app_path: &Path,
    selected_addons: &[String],
) -> Result<(), String> {
    if selected_addons.is_empty() {
        return Ok(());
    }

    let mut all_components: Vec<ComponentInput> = Vec::new();
    for addon_name in selected_addons {
        if let Some(manifest) = read_addon_manifest(addon_name) {
            for comp in &manifest.components.install {
                all_components.push(ComponentInput::new(comp));
            }
        }
    }

    if !all_components.is_empty() {
        let components_start = Instant::now();
        let sp = spinner("🎨 Adding components...");

        let result = add_components(app_path, &all_components, true).await?;

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

    Ok(())
}

/// Print the final success message with instructions.
fn print_success(app_name: &str, workspace_root: &Path, app_path: &Path, is_member: bool) {
    println!();
    println!("✨ Project {app_name} initialized successfully!");
    let run_from = if is_member {
        workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf())
    } else {
        app_path
            .canonicalize()
            .unwrap_or_else(|_| app_path.to_path_buf())
    };
    println!(
        "🚀 Run `cd {} && apx dev start` to get started!",
        run_from.display()
    );
    println!("   (Dependencies will be installed automatically on first run)");
}

fn normalize_app_name(app_name: &str) -> Result<String, String> {
    let normalized = app_name.to_lowercase().replace([' ', '_'], "-");
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(
            "Invalid app name. Please use only alphanumeric characters and dashes.".to_string(),
        );
    }
    Ok(normalized)
}

fn random_name() -> String {
    let adjectives = [
        "fast",
        "simple",
        "clean",
        "elegant",
        "modern",
        "cool",
        "awesome",
        "brave",
        "bold",
        "creative",
        "curious",
        "dynamic",
        "energetic",
        "fantastic",
        "giant",
    ];
    let animals = [
        "lion", "tiger", "bear", "wolf", "fox", "dog", "cat", "bird", "fish", "horse", "rabbit",
        "turtle", "whale", "dolphin", "shark", "octopus",
    ];
    let mut rng = rand::thread_rng();
    let adj = adjectives.choose(&mut rng).unwrap_or(&"fast");
    let animal = animals.choose(&mut rng).unwrap_or(&"lion");
    format!("{adj}-{animal}")
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("Failed to create directory: {err}"))
}

/// Render embedded templates matching `prefix` into `target_dir`.
///
/// The prefix is stripped from the embedded path to form the relative output path.
/// Paths containing `/base/` or starting with `base/` have `base` replaced with `app_slug`.
/// Files ending in `.jinja2` are rendered through Tera; others are copied verbatim.
/// `addon.toml` files are skipped (internal metadata, not user-facing).
pub fn render_embedded_templates(
    prefix: &str,
    target_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<(), String> {
    let files = list_template_files(prefix);
    if files.is_empty() {
        return Err(format!("No template files found for prefix: {prefix}"));
    }

    for file_path in &files {
        // Strip the prefix to get the relative path for the output
        let rel = file_path.strip_prefix(prefix).unwrap_or(file_path.as_str());

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
        let target_path = if is_template {
            let trimmed = path_str.trim_end_matches(".jinja2");
            target_dir.join(trimmed)
        } else {
            target_dir.join(&path_str)
        };

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create directory: {err}"))?;
        }

        let content = get_template_content(file_path)?;

        if is_template {
            let mut context = Context::new();
            context.insert("app_name", app_name);
            context.insert("app_slug", app_slug);
            context.insert(
                "app_letter",
                &app_name.chars().next().unwrap_or('A').to_string(),
            );
            let rendered = tera::Tera::one_off(&content, &context, false).map_err(|err| {
                format!(
                    "Template {file_path} is not tera compatible. Content: {content}\nError: {err}",
                )
            })?;
            fs::write(&target_path, rendered)
                .map_err(|err| format!("Failed to write template output: {err}"))?;
        } else {
            fs::write(&target_path, content.as_bytes())
                .map_err(|err| format!("Failed to write template file: {err}"))?;
        }
    }
    Ok(())
}

/// Programmatically add `[tool.apx.ui]` config and hatch build exclude to pyproject.toml.
/// Idempotent — skips if already configured.
pub fn merge_ui_pyproject_config(app_dir: &Path, app_slug: &str) -> Result<(), String> {
    use toml_edit::{Item, Table};

    let pyproject_path = app_dir.join("pyproject.toml");
    modify_pyproject(&pyproject_path, |doc| {
        let tool = doc["tool"].or_insert(Item::Table(Table::new()));
        let apx = tool["apx"].or_insert(Item::Table(Table::new()));
        let apx_table = apx.as_table_mut().ok_or("tool.apx is not a table")?;

        if apx_table.contains_key("ui") {
            return Ok(()); // Already configured
        }

        let mut ui = Table::new();
        ui["root"] = Item::Value(format!("src/{app_slug}/ui").into());

        let mut registries = Table::new();
        registries["@animate-ui"] = Item::Value("https://animate-ui.com/r/{name}.json".into());
        registries["@ai-elements"] = Item::Value("https://registry.ai-sdk.dev/{name}.json".into());
        registries["@svgl"] = Item::Value("https://svgl.app/r/{name}.json".into());
        ui["registries"] = Item::Table(registries);

        apx_table["ui"] = Item::Table(ui);

        // Add hatch build exclude for UI dir
        let hatch = tool["hatch"].or_insert(Item::Table(Table::new()));
        let build = hatch["build"].or_insert(Item::Table(Table::new()));
        let build_table = build
            .as_table_mut()
            .ok_or("tool.hatch.build is not a table")?;

        let exclude = build_table["exclude"].or_insert(Item::Value(Value::Array(Array::new())));
        let exclude_arr = exclude
            .as_array_mut()
            .ok_or("tool.hatch.build.exclude is not an array")?;

        let ui_exclude = format!("src/{app_slug}/ui");
        let already = exclude_arr
            .iter()
            .any(|v| v.as_str() == Some(ui_exclude.as_str()));
        if !already {
            exclude_arr.push(ui_exclude.as_str());
        }

        Ok(())
    })
}

fn has_git_dir(path: &Path) -> bool {
    for ancestor in path.ancestors() {
        let candidate = ancestor.join(".git");
        if candidate.is_dir() {
            return true;
        }
    }
    false
}

use toml_edit::{Array, Item, Table, Value};

/// Add or update `[tool.uv.workspace]` in a root `pyproject.toml`.
///
/// Derives a glob pattern from the member path (e.g. `packages/app` -> `packages/*`)
/// and ensures it's present in the `members` array. Creates the root `pyproject.toml`
/// if it doesn't exist.
fn ensure_workspace_config(root_pyproject: &Path, member_path: &Path) -> Result<(), String> {
    let member_str = member_path.to_string_lossy().replace('\\', "/");
    let member_glob = match member_str.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/*"),
        None => member_str.clone(),
    };

    if !root_pyproject.exists() {
        let minimal = format!(
            "[project]\nname = \"workspace\"\nversion = \"0.0.0\"\n\n\
             [tool.uv.workspace]\nmembers = [\"{member_glob}\"]\n"
        );
        fs::write(root_pyproject, minimal)
            .map_err(|e| format!("Failed to create root pyproject.toml: {e}"))?;
        debug!("Created root pyproject.toml with workspace config");
        return Ok(());
    }

    modify_pyproject(root_pyproject, |doc| {
        let tool = doc["tool"].or_insert(Item::Table(Table::new()));
        let uv = tool["uv"].or_insert(Item::Table(Table::new()));
        let workspace = uv["workspace"].or_insert(Item::Table(Table::new()));
        let workspace = workspace
            .as_table_mut()
            .ok_or("tool.uv.workspace is not a table")?;

        let members = workspace["members"].or_insert(Item::Value(Value::Array(Array::new())));
        let members = members
            .as_array_mut()
            .ok_or("tool.uv.workspace.members is not an array")?;

        let already_present = members
            .iter()
            .any(|v| v.as_str() == Some(member_glob.as_str()));

        if !already_present {
            members.push(member_glob.as_str());
            debug!("Added workspace member glob: {member_glob}");
        }

        Ok(())
    })
}

#[cfg(test)]
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal pyproject.toml for testing
    fn create_test_pyproject(dir: &Path) -> PathBuf {
        let pyproject_path = dir.join("pyproject.toml");
        let content = r#"[project]
name = "test-app"
version = "0.1.0"

[dependency-groups]
dev = [
    "pytest",
]
"#;
        fs::write(&pyproject_path, content).unwrap();
        pyproject_path
    }

    #[test]
    fn test_normalize_app_name_basic() {
        assert_eq!(normalize_app_name("my-app").unwrap(), "my-app");
        assert_eq!(normalize_app_name("MyApp").unwrap(), "myapp");
        assert_eq!(normalize_app_name("my_app").unwrap(), "my-app");
        assert_eq!(normalize_app_name("my app").unwrap(), "my-app");
    }

    #[test]
    fn test_normalize_app_name_invalid() {
        assert!(normalize_app_name("my@app").is_err());
        assert!(normalize_app_name("my/app").is_err());
        assert!(normalize_app_name("my.app").is_err());
    }

    #[test]
    fn test_ensure_workspace_config_creates_new_file() {
        let dir = TempDir::new().unwrap();
        let pyproject = dir.path().join("pyproject.toml");

        ensure_workspace_config(&pyproject, Path::new("packages/app")).unwrap();

        let content = fs::read_to_string(&pyproject).unwrap();
        assert!(content.contains("[tool.uv.workspace]"));
        assert!(content.contains("packages/*"));
    }

    #[test]
    fn test_ensure_workspace_config_adds_to_existing() {
        let dir = TempDir::new().unwrap();
        let pyproject = create_test_pyproject(dir.path());

        ensure_workspace_config(&pyproject, Path::new("packages/app")).unwrap();

        let content = fs::read_to_string(&pyproject).unwrap();
        assert!(content.contains("[project]"));
        assert!(content.contains("name = \"test-app\""));
        assert!(content.contains("packages/*"));
    }

    #[test]
    fn test_ensure_workspace_config_appends_member() {
        let dir = TempDir::new().unwrap();
        let pyproject = dir.path().join("pyproject.toml");
        let initial = r#"[project]
name = "workspace"

[tool.uv.workspace]
members = ["libs/*"]
"#;
        fs::write(&pyproject, initial).unwrap();

        ensure_workspace_config(&pyproject, Path::new("packages/app")).unwrap();

        let content = fs::read_to_string(&pyproject).unwrap();
        assert!(content.contains("libs/*"));
        assert!(content.contains("packages/*"));
    }

    #[test]
    fn test_ensure_workspace_config_idempotent() {
        let dir = TempDir::new().unwrap();
        let pyproject = create_test_pyproject(dir.path());

        ensure_workspace_config(&pyproject, Path::new("packages/app")).unwrap();
        ensure_workspace_config(&pyproject, Path::new("packages/app")).unwrap();

        let content = fs::read_to_string(&pyproject).unwrap();
        assert_eq!(content.matches("packages/*").count(), 1);
    }

    #[test]
    fn test_ensure_workspace_config_derives_glob() {
        let dir = TempDir::new().unwrap();
        let pyproject = dir.path().join("pyproject.toml");

        ensure_workspace_config(&pyproject, Path::new("src/apps/my-app")).unwrap();

        let content = fs::read_to_string(&pyproject).unwrap();
        assert!(content.contains("src/apps/*"), "content: {content}");
    }

    #[test]
    fn test_merge_ui_pyproject_config() {
        let dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(dir.path());

        // Add [tool.apx.metadata] first (like a real init would)
        modify_pyproject(&pyproject_path, |doc| {
            let tool = doc["tool"].or_insert(Item::Table(Table::new()));
            let apx = tool["apx"].or_insert(Item::Table(Table::new()));
            let apx_table = apx.as_table_mut().ok_or("not a table")?;
            let mut meta = Table::new();
            meta["app-name"] = Item::Value("test-app".into());
            apx_table["metadata"] = Item::Table(meta);

            let hatch = tool["hatch"].or_insert(Item::Table(Table::new()));
            let build = hatch["build"].or_insert(Item::Table(Table::new()));
            let build_table = build.as_table_mut().ok_or("not a table")?;
            build_table["artifacts"] = Item::Value(Value::Array(Array::new()));
            Ok(())
        })
        .unwrap();

        merge_ui_pyproject_config(dir.path(), "test_app").unwrap();

        let content = fs::read_to_string(&pyproject_path).unwrap();
        assert!(content.contains("[tool.apx.ui]"));
        assert!(content.contains("src/test_app/ui"));
        assert!(content.contains("@animate-ui"));
        assert!(content.contains("src/test_app/ui")); // in exclude
    }

    #[test]
    fn test_merge_ui_pyproject_config_idempotent() {
        let dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(dir.path());

        modify_pyproject(&pyproject_path, |doc| {
            let tool = doc["tool"].or_insert(Item::Table(Table::new()));
            let apx = tool["apx"].or_insert(Item::Table(Table::new()));
            let apx_table = apx.as_table_mut().ok_or("not a table")?;
            let mut meta = Table::new();
            meta["app-name"] = Item::Value("test-app".into());
            apx_table["metadata"] = Item::Table(meta);
            Ok(())
        })
        .unwrap();

        merge_ui_pyproject_config(dir.path(), "test_app").unwrap();
        merge_ui_pyproject_config(dir.path(), "test_app").unwrap();

        let content = fs::read_to_string(&pyproject_path).unwrap();
        assert_eq!(content.matches("[tool.apx.ui]").count(), 1);
    }
}
