pub mod add;
pub mod cache;
pub mod css_updater;
pub mod models;
pub mod tw_transform;
pub mod utils;

// Re-export models for easier access
pub use models::{
    CssRules, RegistryCatalogEntry, RegistryConfig, RegistryFile, RegistryItem, UiConfig,
};

// Re-export cache functions
pub use cache::{
    SharedCacheState, get_all_registry_indexes, needs_registry_refresh, new_cache_state,
    sync_registry_indexes,
};

use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, warn};
use url::Url;

use crate::components::css_updater::{CssMutation, CssUpdater};
use crate::components::models::TailwindConfig;

/// Scan planned component files for 3rd-party npm imports that may not be listed
/// in the registry spec's `dependencies` array.
///
/// Returns a deduplicated, sorted list of bare package names (e.g. `"ai"`, `"streamdown"`).
///
/// Filters out:
/// - Relative imports (`./`, `../`)
/// - Alias/path imports (`@/`)
/// - Common always-present packages (`react`, `react-dom`, `next`)
pub fn detect_external_imports(files: &[PlannedFile]) -> Vec<String> {
    let always_present: HashSet<&str> = ["react", "react-dom", "next"].into_iter().collect();

    let mut packages: BTreeSet<String> = BTreeSet::new();

    for file in files {
        // Only scan .ts/.tsx/.js/.jsx files
        let ext = file
            .relative_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !matches!(ext, "ts" | "tsx" | "js" | "jsx") {
            continue;
        }

        for specifier in extract_import_specifiers(&file.content) {
            // Skip relative imports
            if specifier.starts_with('.') {
                continue;
            }

            // Skip project alias imports (@/ is a path alias, not an npm scope)
            if specifier.starts_with("@/") {
                continue;
            }

            // Extract bare package name:
            // - "ai" → "ai"
            // - "streamdown/react" → "streamdown"
            // - "@scope/pkg" → "@scope/pkg"
            // - "@scope/pkg/sub" → "@scope/pkg"
            let package_name = if specifier.starts_with('@') {
                // Scoped package: @scope/name or @scope/name/subpath
                let parts: Vec<&str> = specifier.splitn(3, '/').collect();
                if parts.len() >= 2 {
                    format!("{}/{}", parts[0], parts[1])
                } else {
                    continue; // malformed
                }
            } else {
                // Unscoped: name or name/subpath
                specifier
                    .split('/')
                    .next()
                    .unwrap_or(&specifier)
                    .to_string()
            };

            if always_present.contains(package_name.as_str()) {
                continue;
            }

            packages.insert(package_name);
        }
    }

    packages.into_iter().collect()
}

/// Extract import specifiers from TypeScript/JavaScript content.
///
/// Finds patterns like:
/// - `from "module"`  / `from 'module'`
/// - `import "module"` / `import 'module'` (side-effect imports)
fn extract_import_specifiers(content: &str) -> Vec<String> {
    let mut specifiers = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Look for `from "..."` or `from '...'` patterns
        if let Some(pos) = trimmed.find("from ") {
            let after_from = &trimmed[pos + 5..].trim_start();
            if let Some(spec) = extract_quoted_string(after_from) {
                specifiers.push(spec);
                continue;
            }
        }

        // Look for side-effect imports: `import "..."` or `import '...'`
        if let Some(after_import_kw) = trimmed.strip_prefix("import ") {
            let after_import = after_import_kw.trim_start();
            // Only match if the next thing is a quote (side-effect import)
            if (after_import.starts_with('"') || after_import.starts_with('\''))
                && let Some(spec) = extract_quoted_string(after_import)
            {
                specifiers.push(spec);
            }
        }
    }

    specifiers
}

/// Extract a quoted string value from text starting with a quote character.
fn extract_quoted_string(s: &str) -> Option<String> {
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &s[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Default shadcn/ui registry item template.
///
/// IMPORTANT: /r/{name}.json is 404.
/// The working endpoints are style-scoped:
///   https://ui.shadcn.com/r/styles/{style}/{name}.json
/// Example:
///   https://ui.shadcn.com/r/styles/new-york/button.json
pub const SHADCN_REGISTRY_ITEM_TEMPLATE: &str =
    "https://ui.shadcn.com/r/styles/{style}/{name}.json";

/// Retry configuration for HTTP requests
const MAX_RETRIES: u32 = 5;
const INITIAL_DELAY_MS: u64 = 125;

/// Execute an async operation with exponential backoff retry.
///
/// Retries up to 5 times with delays: 125ms, 250ms, 500ms, 1000ms, 2000ms (~4 seconds total).
async fn fetch_with_retry<T, F, Fut>(operation: F, operation_name: &str) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_error = String::new();
    for attempt in 0..MAX_RETRIES {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = e;
                if attempt < MAX_RETRIES - 1 {
                    let delay = INITIAL_DELAY_MS * (1 << attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        delay_ms = delay,
                        operation = operation_name,
                        error = %last_error,
                        "HTTP request failed, retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }
    Err(format!(
        "{operation_name}: {last_error} (after {MAX_RETRIES} retries)"
    ))
}

pub async fn fetch_registry_catalog_impl(
    client: &reqwest::Client,
) -> Result<Vec<RegistryCatalogEntry>, String> {
    // Try cache first
    if let Ok(Some(catalog)) = cache::load_cached_registry_catalog() {
        return Ok(catalog);
    }

    // HTTP fetch with retry
    let url = "https://ui.shadcn.com/r/registries.json";
    let catalog = fetch_with_retry(
        || async {
            client
                .get(url)
                .send()
                .await
                .map_err(|e| format!("Failed to fetch registry catalog: {e}"))?
                .error_for_status()
                .map_err(|e| format!("Registry catalog returned error: {e}"))?
                .json::<Vec<RegistryCatalogEntry>>()
                .await
                .map_err(|e| format!("Invalid registry catalog JSON: {e}"))
        },
        "fetch registry catalog",
    )
    .await?;

    // Save to cache (non-fatal on error)
    let _ = cache::save_cached_registry_catalog(&catalog);

    Ok(catalog)
}

pub fn merge_registries(
    local: &HashMap<String, RegistryConfig>,
    discovered: &[RegistryCatalogEntry],
) -> HashMap<String, RegistryConfig> {
    let mut merged: HashMap<String, RegistryConfig> = discovered
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                RegistryConfig::Template(entry.url.clone()),
            )
        })
        .collect();

    for (name, config) in local {
        merged.insert(name.clone(), config.clone());
    }

    merged
}

#[derive(Debug)]
pub struct ResolvedComponent {
    pub name: String,
    pub spec: RegistryItem,
    pub registry: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct AddPlan {
    pub components: Vec<ResolvedComponent>,
    pub files_to_write: Vec<PlannedFile>,
    pub component_deps: BTreeSet<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct PlannedFile {
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub content: String,
    pub source_component: String,
}

/// Request resolved from a registry definition (URL + optional headers/params).
#[derive(Debug, Clone)]
pub struct ResolvedRequest {
    pub url: Url,
    pub headers: HashMap<String, String>,
}

/// Resolve a component spec request.
///
/// Behavior:
/// - If `component` is a full URL: use it directly.
/// - If `registry` is None: use shadcn default template with {style}.
/// - Else: look up registry in UiConfig registries and resolve {name}, {style},
pub fn resolve_component_request(
    cfg: &UiConfig,
    registry: Option<&str>,
    component: &str,
) -> Result<ResolvedRequest, String> {
    debug!(
        registry = ?registry,
        component = component,
        available_registries = ?cfg.registries.keys().collect::<Vec<_>>(),
        "Starting component request resolution"
    );

    // 1) Explicit URL provided
    if component.starts_with("http://") || component.starts_with("https://") {
        debug!(url = component, "Component is a direct URL");
        return Ok(ResolvedRequest {
            url: Url::parse(component).map_err(|e| format!("Invalid URL: {e}"))?,
            headers: HashMap::new(),
        });
    }

    let style = cfg.style();

    // 2) Default registry: shadcn/ui
    if registry.is_none() {
        let url_candidate = SHADCN_REGISTRY_ITEM_TEMPLATE
            .replace("{style}", style)
            .replace("{name}", component);

        let url = Url::parse(&url_candidate).map_err(|e| format!("Invalid URL: {e}"))?;

        debug!(
            component = component,
            style = style,
            url = url_candidate,
            "Resolving via default shadcn registry"
        );

        return Ok(ResolvedRequest {
            url: url.clone(),
            headers: HashMap::new(),
        });
    }

    // 3) Named registry from UiConfig
    // SAFETY: registry is guaranteed to be Some here because we returned early if it was None
    let Some(registry_name) = registry else {
        unreachable!("registry cannot be None here due to early return above")
    };

    debug!(
        registry_name = registry_name,
        "Looking up named registry in UiConfig"
    );

    let reg = cfg
        .registries
        .get(registry_name)
        .ok_or_else(|| {
            let available: Vec<&String> = cfg.registries.keys().collect();
            format!("Unknown registry: {registry_name}. Available registries: {available:?}")
        })?
        .clone();

    match reg {
        RegistryConfig::Template(tpl) => {
            let url_candidate = apply_placeholders(&tpl, component, style)?;
            let url = Url::parse(&url_candidate).map_err(|e| format!("Invalid URL: {e}"))?;
            debug!(
                registry = registry_name,
                component = component,
                style = style,
                url = url.as_str(),
                "Resolving via template registry"
            );

            Ok(ResolvedRequest {
                url: url.clone(),
                headers: HashMap::new(),
            })
        }
        RegistryConfig::Advanced(adv) => {
            // 1. Expand placeholders before URL parsing
            let url_candidate = apply_placeholders(&adv.url, component, style)?;
            let mut url = Url::parse(&url_candidate).map_err(|e| format!("Invalid URL: {e}"))?;

            // 2. Append params via url::Url (handles encoding & ?/& correctly)
            if !adv.params.is_empty() {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in &adv.params {
                    let k = expand_env(k)?;
                    let v = expand_env(v)?;
                    pairs.append_pair(&k, &v);
                }
                // `pairs` is committed when dropped
            }

            // 3. Headers (env expanded)
            let mut headers = HashMap::new();
            for (k, v) in &adv.headers {
                headers.insert(expand_env(k)?, expand_env(v)?);
            }

            debug!(
                registry = registry_name,
                component = component,
                style = style,
                url = %url,
                headers_len = headers.len(),
                "Resolving via advanced registry"
            );

            Ok(ResolvedRequest { url, headers })
        }
    }
}

pub async fn fetch_component_impl(
    client: &reqwest::Client,
    req: &ResolvedRequest,
    registry_name: Option<&str>,
    component_name: Option<&str>,
) -> Result<(RegistryItem, Vec<String>), String> {
    // Try cache first if we have component name
    if let Some(component_name_val) = component_name
        && let Ok(Some((item, warnings))) =
            cache::load_cached_component(component_name_val, registry_name)
    {
        return Ok((item, warnings));
    }

    // Direct fetch (original implementation)
    let result = match req.url.scheme() {
        "http" | "https" => fetch_http_component(client, req).await?,
        "file" => fetch_file_component(req).await?,
        scheme => return Err(format!("Unsupported registry URL scheme: {scheme}")),
    };

    // Save to cache if we have component name
    if let Some(component_name_val) = component_name {
        let _ =
            cache::save_cached_component(component_name_val, registry_name, &result.0, &result.1);
    }

    Ok(result)
}

/// Fetch component spec, applying headers from resolved request.
pub(crate) async fn fetch_http_component(
    client: &reqwest::Client,
    req: &ResolvedRequest,
) -> Result<(RegistryItem, Vec<String>), String> {
    let url = req.url.clone();
    let headers = req.headers.clone();
    let url_str = url.to_string();

    // HTTP fetch with retry
    let value = fetch_with_retry(
        || {
            let url = url.clone();
            let headers = headers.clone();
            async move {
                let mut rb = client.get(url);
                for (k, v) in &headers {
                    rb = rb.header(k, v);
                }
                rb.send()
                    .await
                    .map_err(|e| format!("Failed to fetch component: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("Registry returned error: {e}"))?
                    .json::<serde_json::Value>()
                    .await
                    .map_err(|e| format!("Invalid component spec: {e}"))
            }
        },
        &format!("fetch component from {url_str}"),
    )
    .await?;

    let warnings = detect_forbidden_fields(&value);

    let item: RegistryItem =
        serde_json::from_value(value).map_err(|e| format!("Invalid component spec: {e}"))?;

    validate_registry_item(&item)?;

    Ok((item, warnings))
}

async fn fetch_file_component(
    req: &ResolvedRequest,
) -> Result<(RegistryItem, Vec<String>), String> {
    let path = req
        .url
        .to_file_path()
        .map_err(|_| format!("Invalid file URL: {}", req.url))?;

    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read registry file {}: {e}", path.display()))?;

    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid component spec: {e}"))?;

    let warnings = detect_forbidden_fields(&value);

    let item: RegistryItem =
        serde_json::from_value(value).map_err(|e| format!("Invalid component spec: {e}"))?;

    validate_registry_item(&item)?;

    Ok((item, warnings))
}

pub async fn resolve_component_closure(
    client: &reqwest::Client,
    cfg: &UiConfig,
    registry: Option<&str>,
    root_component: &str,
) -> Result<Vec<ResolvedComponent>, String> {
    debug!(
        registry = ?registry,
        component = root_component,
        "Starting component closure resolution"
    );

    #[derive(Clone)]
    enum VisitState {
        Enter,
        Exit,
    }

    let mut stack: Vec<(VisitState, Option<String>, String)> = vec![(
        VisitState::Enter,
        registry.map(|value| value.to_string()),
        root_component.to_string(),
    )];
    let mut visited: HashSet<String> = HashSet::new();
    let mut specs: HashMap<String, (RegistryItem, Option<String>, Vec<String>)> = HashMap::new();
    let mut ordered: Vec<ResolvedComponent> = Vec::new();
    let mut component_deps: BTreeSet<String> = BTreeSet::new();

    while let Some((state, current_registry, component)) = stack.pop() {
        let key = format!(
            "{}::{}",
            current_registry
                .clone()
                .unwrap_or_else(|| "_default".to_string()),
            component
        );

        match state {
            VisitState::Enter => {
                if visited.contains(&key) {
                    continue;
                }
                visited.insert(key.clone());

                debug!(
                    component = component.as_str(),
                    registry = ?current_registry,
                    "Resolving component in closure"
                );

                let req = resolve_component_request(
                    cfg,
                    current_registry.as_deref(),
                    component.as_str(),
                )?;

                debug!(
                    url = req.url.as_str(),
                    headers_count = req.headers.len(),
                    "Resolved component request"
                );

                let (spec, warnings) = fetch_component_impl(
                    client,
                    &req,
                    current_registry.as_deref(),
                    Some(component.as_str()),
                )
                .await?;

                for dep in &spec.dependencies {
                    component_deps.insert(dep.to_string());
                }

                stack.push((
                    VisitState::Exit,
                    current_registry.clone(),
                    component.clone(),
                ));
                specs.insert(key.clone(), (spec, current_registry.clone(), warnings));

                if let Some((spec, _, _)) = specs.get(&key) {
                    for dep in &spec.registry_dependencies {
                        let (dep_registry, dep_component) =
                            parse_registry_dependency(dep, current_registry.as_deref());
                        let dep_key = format!(
                            "{}::{}",
                            dep_registry
                                .clone()
                                .unwrap_or_else(|| "_default".to_string()),
                            dep_component
                        );
                        if !visited.contains(&dep_key) {
                            stack.push((VisitState::Enter, dep_registry, dep_component));
                        }
                    }
                }
            }
            VisitState::Exit => {
                if let Some((spec, spec_registry, warnings)) = specs.remove(&key) {
                    ordered.push(ResolvedComponent {
                        name: spec.name.clone(),
                        spec,
                        registry: spec_registry,
                        warnings,
                    });
                }
            }
        }
    }

    if !component_deps.is_empty() {
        debug!(
            count = component_deps.len(),
            "Collected component dependencies"
        );
    }

    Ok(ordered)
}

pub async fn plan_add(
    client: &reqwest::Client,
    _app_dir: &Path,
    cfg: &UiConfig,
    registry: Option<&str>,
    component: &str,
) -> Result<AddPlan, String> {
    debug!(
        registry = ?registry,
        component = component,
        "Planning component addition"
    );

    let components_base_dir = cfg.components_dir();
    let lib_base_dir = cfg.lib_dir();
    let hooks_base_dir = cfg.hooks_dir();

    let discovered = fetch_registry_catalog_impl(client).await?;
    let merged_registries = merge_registries(&cfg.registries, &discovered);

    debug!(
        local_registries = ?cfg.registries.keys().collect::<Vec<_>>(),
        discovered_count = discovered.len(),
        merged_count = merged_registries.len(),
        "Registry merge complete"
    );

    // print CSS path
    debug!(css_path = ?cfg.css_path(), "CSS file path loaded");

    let merged_cfg = UiConfig {
        root: cfg.root.clone(),
        registries: merged_registries,
    };

    let components = resolve_component_closure(client, &merged_cfg, registry, component).await?;

    let path_map = build_path_map(&components);

    let mut files_to_write = Vec::new();
    let mut component_deps: BTreeSet<String> = BTreeSet::new();
    let mut warnings: Vec<String> = Vec::new();

    for resolved in &components {
        warnings.extend(resolved.warnings.clone());
        for dep in &resolved.spec.dependencies {
            component_deps.insert(dep.to_string());
        }

        for file in &resolved.spec.files {
            let root = determine_output_root(file.file_type.as_deref());

            let file_name = match root {
                OutputRoot::Components => format!("{}.tsx", resolved.name),
                OutputRoot::Lib | OutputRoot::Hooks => format!("{}.ts", resolved.name),
            };
            let (relative_path, absolute_path) = match root {
                OutputRoot::Components => {
                    let registry = resolved
                        .registry
                        .as_deref()
                        .map(|r| r.trim_start_matches('@'));

                    let subdir = match registry {
                        None => "ui",
                        Some(name) => name,
                    };

                    let relative_path = PathBuf::from(subdir).join(file_name);
                    let absolute_path = components_base_dir.join(&relative_path);
                    (relative_path, absolute_path)
                }
                OutputRoot::Lib => {
                    let relative_path = PathBuf::from(file_name);
                    let absolute_path = lib_base_dir.join(&relative_path);
                    (relative_path, absolute_path)
                }
                OutputRoot::Hooks => {
                    let relative_path = PathBuf::from(file_name);
                    let absolute_path = hooks_base_dir.join(&relative_path);
                    (relative_path, absolute_path)
                }
            };

            files_to_write.push(PlannedFile {
                relative_path,
                absolute_path,
                content: rewrite_flattened_paths(
                    &rewrite_registry_imports(&file.content),
                    &path_map,
                ),
                source_component: resolved.name.clone(),
            });
        }
    }

    Ok(AddPlan {
        components,
        files_to_write,
        component_deps,
        warnings,
    })
}

/// Where a file ends up: components dir, lib dir, or hooks dir.
enum OutputRoot {
    Components,
    Lib,
    Hooks,
}

fn determine_output_root(file_type: Option<&str>) -> OutputRoot {
    match file_type {
        Some("registry:ui") => OutputRoot::Components,
        Some("registry:hook") => OutputRoot::Hooks,
        Some("registry:lib") | Some("registry:file") => OutputRoot::Lib,
        _ => OutputRoot::Components,
    }
}

/// Returns the `@/...` TypeScript import path where `file` will actually be saved.
///
/// This reflects the *flattened* output name derived from `resolved.name`, not `file.path`.
fn compute_saved_import_path(resolved: &ResolvedComponent, file: &RegistryFile) -> String {
    match determine_output_root(file.file_type.as_deref()) {
        OutputRoot::Components => {
            let registry = resolved
                .registry
                .as_deref()
                .map(|r| r.trim_start_matches('@'));
            let subdir = registry.unwrap_or("ui");
            format!("@/components/{}/{}", subdir, resolved.name)
        }
        OutputRoot::Hooks => format!("@/hooks/{}", resolved.name),
        OutputRoot::Lib => format!("@/lib/{}", resolved.name),
    }
}

/// Converts a registry `file.path` (e.g. `"components/animate-ui/icons/icon.tsx"`)
/// to the TypeScript import path that the *original* nested layout would use
/// (e.g. `"@/components/animate-ui/icons/icon"`).
fn original_registry_import_path(file_path: &str) -> String {
    let without_ext = file_path
        .strip_suffix(".tsx")
        .or_else(|| file_path.strip_suffix(".ts"))
        .or_else(|| file_path.strip_suffix(".jsx"))
        .or_else(|| file_path.strip_suffix(".js"))
        .unwrap_or(file_path);
    format!("@/{without_ext}")
}

/// Build a map from original nested import paths to the flattened saved paths.
///
/// Only entries where the two paths differ are inserted.
fn build_path_map(components: &[ResolvedComponent]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for resolved in components {
        for file in &resolved.spec.files {
            let original = original_registry_import_path(&file.path);
            let saved = compute_saved_import_path(resolved, file);
            if original != saved {
                map.insert(original, saved);
            }
        }
    }
    map
}

/// Rewrite import paths in `content` according to `path_map`.
///
/// Replacements are anchored to a following quote character (`"`, `'`, `` ` ``) to avoid
/// partial-prefix false positives (e.g. `@/hooks/icon` should not match `@/hooks/icon-button`).
/// Entries are applied longest-first so that more-specific paths win.
fn rewrite_flattened_paths(content: &str, path_map: &HashMap<String, String>) -> String {
    if path_map.is_empty() {
        return content.to_string();
    }
    let mut sorted: Vec<(&String, &String)> = path_map.iter().collect();
    sorted.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let mut result = content.to_string();
    for (from, to) in sorted {
        for quote in ['"', '\'', '`'] {
            let pat = format!("{from}{quote}");
            let rep = format!("{to}{quote}");
            result = result.replace(&pat, &rep);
        }
    }
    result
}

/// Rewrite imports from shadcn default registry structure to project structure.
///
/// Only handles the default shadcn registry case:
/// - `@/registry/{style}/ui/button` → first pass → `@/ui/button` → second pass → `@/components/ui/button`
/// - `@/registry/{style}/hooks/use-mobile` → `@/hooks/use-mobile` (correct after first pass)
/// - `@/registry/{style}/lib/utils` → `@/lib/utils` (correct after first pass)
fn rewrite_registry_imports(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut remaining = content;

    // First pass: Strip @/registry/{style}/ → @/
    while let Some(pos) = remaining.find("@/registry/") {
        result.push_str(&remaining[..pos]);
        let after_prefix = &remaining[pos + "@/registry/".len()..];

        // Find the next '/' which marks the end of the style name
        if let Some(slash_pos) = after_prefix.find('/') {
            // Skip the style name and the slash, replace with "@/"
            result.push_str("@/");
            remaining = &after_prefix[slash_pos + 1..];
        } else {
            // No slash found, just copy the prefix and continue
            result.push_str("@/registry/");
            remaining = after_prefix;
        }
    }
    result.push_str(remaining);

    // Second pass: Transform @/ui/ → @/components/ui/
    // This handles the case where shadcn components import from "@/ui/..." shorthand
    let result = result.replace("@/ui/", "@/components/ui/");

    // Third pass: Transform Tailwind v3 class syntax to v4
    tw_transform::transform_tailwind_v3_to_v4(&result)
}

fn apply_placeholders(template: &str, name: &str, style: &str) -> Result<String, String> {
    if !template.contains("{name}") {
        return Err("Registry template missing {name} placeholder".to_string());
    }
    let mut url = template.to_string();
    url = url.replace("{style}", style);
    url = url.replace("{name}", name);
    Ok(url)
}

/// Expand ${VAR_NAME} from process environment.
fn expand_env(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && (i + 1) < bytes.len() && bytes[i + 1] == b'{' {
            // parse ${...}
            i += 2;
            let start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            let key = &s[start..i.min(s.len())];
            // skip '}'
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            let val =
                std::env::var(key).map_err(|_| format!("Missing environment variable `{key}`"))?;
            out.push_str(&val);
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    Ok(out)
}

fn parse_registry_dependency(
    dep: &str,
    current_registry: Option<&str>,
) -> (Option<String>, String) {
    if dep.starts_with("http://") || dep.starts_with("https://") {
        return (None, dep.to_string());
    }

    // Explicit registry always wins: "@animate-ui/foo"
    if dep.starts_with('@')
        && let Some((registry, name)) = dep.split_once('/')
    {
        return (Some(registry.to_string()), name.to_string());
    }

    // Unqualified deps should resolve from the default registry.
    // This is crucial for 3rd-party registries that depend on shadcn primitives
    // like "button", "input", "use-mobile", etc.
    //
    // If a 3rd-party registry wants an internal dep, it should qualify it via "@registry/name".
    let _ = current_registry; // keep param for now (may be useful for future fallback logic)
    (None, dep.to_string())
}

fn detect_forbidden_fields(value: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return warnings,
    };
    let name = obj
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("registry item");

    if obj.contains_key("envVars") {
        warnings.push(format!(
            "Registry item `{name}` includes unsupported `envVars`. Ignoring envVars contents."
        ));
    }

    // Note: tailwind, cssVars and css are now automatically applied via apply_css_updates
    // tailwind config is converted to Tailwind v4 CSS format automatically

    warnings
}

fn validate_registry_item(item: &RegistryItem) -> Result<(), String> {
    debug!(
        name = %item.name,
        title = ?item.title,
        description = ?item.description,
        item_type = ?item.item_type,
        css_vars = ?item.css_vars,
        css = ?item.css,
        docs = ?item.docs,
        categories = ?item.categories,
        meta = ?item.meta,
        "Validating registry item"
    );

    if item.name.trim().is_empty() {
        return Err("Registry item `name` is required".to_string());
    }

    if item.files.is_empty() && item.css_vars.is_none() && item.css.is_none() {
        return Err(format!(
            "Registry item `{}` has no files and no CSS effects",
            item.name
        ));
    }

    for file in &item.files {
        if file.path.trim().is_empty() {
            return Err(format!(
                "Registry item `{}` has a file with empty path",
                item.name
            ));
        }
        if file.content.trim().is_empty() {
            return Err(format!(
                "Registry item `{}` file `{}` is missing content",
                item.name, file.path
            ));
        }
    }
    Ok(())
}

fn render_css_rules(css: &CssRules) -> Result<String, String> {
    let mut out = String::new();
    for (selector, value) in css {
        render_css_rule(&mut out, selector, value, 0)?;
    }
    Ok(out.trim().to_string())
}

fn render_css_rule(
    out: &mut String,
    selector: &str,
    value: &Value,
    indent: usize,
) -> Result<(), String> {
    let map = value.as_object().ok_or_else(|| {
        format!("CSS rule `{selector}` must be an object of declarations or nested rules")
    })?;

    if map.is_empty() {
        out.push_str(&" ".repeat(indent));
        out.push_str(selector);
        out.push_str(";\n");
        return Ok(());
    }

    if map.values().all(is_declaration_value) {
        out.push_str(&" ".repeat(indent));
        out.push_str(selector);
        out.push_str(" {\n");
        for (prop, prop_value) in map {
            let value = render_declaration_value(prop_value)?;
            out.push_str(&" ".repeat(indent + 2));
            out.push_str(prop);
            out.push_str(": ");
            out.push_str(&value);
            out.push_str(";\n");
        }
        out.push_str(&" ".repeat(indent));
        out.push_str("}\n");
        return Ok(());
    }

    out.push_str(&" ".repeat(indent));
    out.push_str(selector);
    out.push_str(" {\n");
    for (nested_selector, nested_value) in map {
        render_css_rule(out, nested_selector, nested_value, indent + 2)?;
    }
    out.push_str(&" ".repeat(indent));
    out.push_str("}\n");
    Ok(())
}

fn is_declaration_value(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn render_declaration_value(value: &Value) -> Result<String, String> {
    match value {
        Value::String(val) => Ok(val.clone()),
        Value::Number(val) => Ok(val.to_string()),
        Value::Bool(val) => Ok(val.to_string()),
        _ => Err("CSS declaration values must be string, number, or bool".to_string()),
    }
}

pub fn apply_css_updates(css_path: &Path, mutations: Vec<CssMutation>) -> Result<(), String> {
    let source =
        std::fs::read_to_string(css_path).map_err(|e| format!("Failed to read CSS file: {e}"))?;
    let mut updater = CssUpdater::new(&source).map_err(|e| format!("Failed to parse CSS: {e}"))?;
    if updater
        .apply(&mutations)
        .map_err(|e| format!("Failed to apply CSS updates: {e}"))?
    {
        std::fs::write(css_path, updater.finish())
            .map_err(|e| format!("Failed to write CSS file: {e}"))?;
    }
    Ok(())
}

/// Convert deprecated Tailwind v3 config to Tailwind v4 CSS mutations.
///
/// Handles:
/// - `theme.extend.colors` -> `@theme inline { --color-{name}: value; }`
/// - `theme.extend.keyframes` -> `@keyframes { ... }`
/// - `theme.extend.animation` -> `@theme inline { --animate-{name}: value; }`
/// - `theme.extend.fontFamily` -> `@theme inline { --font-{name}: value; }`
/// - `theme.extend.borderRadius` -> `@theme inline { --radius-{name}: value; }`
/// - `theme.extend.spacing` -> `@theme inline { --spacing-{name}: value; }`
fn convert_tailwind_to_mutations(tailwind: &TailwindConfig, mutations: &mut Vec<CssMutation>) {
    let Some(ref config) = tailwind.config else {
        return;
    };
    let Some(ref theme) = config.theme else {
        return;
    };
    let Some(ref extend) = theme.extend else {
        return;
    };

    let mut theme_vars = Vec::new();

    // Convert colors to @theme inline mappings
    // Handles both simple and nested formats:
    // Simple: { "brand": "hsl(var(--brand))" } -> --color-brand: hsl(var(--brand));
    // Nested: { "sidebar": { "DEFAULT": "...", "foreground": "..." } } -> --color-sidebar: ...; --color-sidebar-foreground: ...;
    for (color_name, value) in &extend.colors {
        match value {
            // Nested format: { "DEFAULT": "...", "foreground": "..." }
            Value::Object(variants) => {
                for (variant, val) in variants {
                    if let Some(val_str) = val.as_str() {
                        let var_name = if variant == "DEFAULT" {
                            format!("--color-{color_name}")
                        } else {
                            format!("--color-{color_name}-{variant}")
                        };
                        theme_vars.push((var_name, val_str.to_string()));
                    }
                }
            }
            // Simple format: "hsl(var(--brand))"
            Value::String(val_str) => {
                theme_vars.push((format!("--color-{color_name}"), val_str.clone()));
            }
            _ => {}
        }
    }

    // Convert animations to @theme inline mappings
    // e.g., { "accordion-down": "accordion-down 0.2s ease-out" }
    // becomes: --animate-accordion-down: accordion-down 0.2s ease-out;
    for (name, value) in &extend.animation {
        theme_vars.push((format!("--animate-{name}"), value.clone()));
    }

    // Convert fontFamily to @theme inline mappings
    // e.g., { "heading": ["Poppins", "sans-serif"] } or { "heading": "Poppins, sans-serif" }
    // becomes: --font-heading: Poppins, sans-serif;
    for (name, value) in &extend.font_family {
        let font_value = match value {
            Value::Array(fonts) => fonts
                .iter()
                .filter_map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            Value::String(s) => s.clone(),
            _ => continue,
        };
        if !font_value.is_empty() {
            theme_vars.push((format!("--font-{name}"), font_value));
        }
    }

    // Convert borderRadius to @theme inline mappings
    // e.g., { "custom": "0.5rem" } -> --radius-custom: 0.5rem;
    for (name, value) in &extend.border_radius {
        theme_vars.push((format!("--radius-{name}"), value.clone()));
    }

    // Convert spacing to @theme inline mappings
    // e.g., { "custom": "2rem" } -> --spacing-custom: 2rem;
    for (name, value) in &extend.spacing {
        theme_vars.push((format!("--spacing-{name}"), value.clone()));
    }

    // Add all theme vars as a single mutation
    if !theme_vars.is_empty() {
        mutations.push(CssMutation::AddThemeMappings { vars: theme_vars });
    }

    // Convert keyframes to @keyframes blocks
    // e.g., { "accordion-down": { "from": { "height": "0" }, "to": { "height": "var(...)" } } }
    // Also handles percentage selectors like "0%, 100%"
    for (keyframe_name, frames) in &extend.keyframes {
        let body = render_keyframes(frames);
        if !body.is_empty() {
            mutations.push(CssMutation::AddCssBlock {
                at_rule: format!("@keyframes {keyframe_name}"),
                body,
            });
        }
    }
}

/// Render keyframe frames to CSS
fn render_keyframes(frames: &HashMap<String, Value>) -> String {
    let mut out = String::new();
    for (selector, props) in frames {
        out.push_str("  ");
        out.push_str(selector);
        out.push_str(" {\n");
        if let Some(obj) = props.as_object() {
            for (prop, value) in obj {
                if let Some(val_str) = value.as_str() {
                    out.push_str("    ");
                    out.push_str(prop);
                    out.push_str(": ");
                    out.push_str(val_str);
                    out.push_str(";\n");
                }
            }
        }
        out.push_str("  }\n");
    }
    out
}

/// Collect CSS mutations from registry items
pub fn collect_css_mutations(components: &[ResolvedComponent]) -> Vec<CssMutation> {
    let mut mutations = Vec::new();

    for resolved in components {
        // Convert cssVars to mutations
        if let Some(ref css_vars) = resolved.spec.css_vars {
            // Theme vars
            if !css_vars.theme.is_empty() {
                let vars: Vec<(String, String)> = css_vars
                    .theme
                    .iter()
                    .map(|(k, v)| (format!("--{k}"), v.clone()))
                    .collect();
                mutations.push(CssMutation::AddThemeMappings { vars });
            }

            // Light vars (:root)
            if !css_vars.light.is_empty() {
                let vars: Vec<(String, String)> = css_vars
                    .light
                    .iter()
                    .map(|(k, v)| (format!("--{k}"), v.clone()))
                    .collect();
                mutations.push(CssMutation::AddCssVars {
                    selector: ":root".to_string(),
                    vars,
                });
            }

            // Dark vars (.dark)
            if !css_vars.dark.is_empty() {
                let vars: Vec<(String, String)> = css_vars
                    .dark
                    .iter()
                    .map(|(k, v)| (format!("--{k}"), v.clone()))
                    .collect();
                mutations.push(CssMutation::AddCssVars {
                    selector: ".dark".to_string(),
                    vars,
                });
            }
        }

        // Convert css rules to mutations
        if let Some(ref css_rules) = resolved.spec.css {
            // For now, convert CSS rules to a single @layer base block
            // This matches shadcn's typical pattern
            if !css_rules.is_empty() {
                match render_css_rules(css_rules) {
                    Ok(rendered) if !rendered.is_empty() => {
                        mutations.push(CssMutation::AddCssBlock {
                            at_rule: "@layer base".to_string(),
                            body: rendered,
                        });
                    }
                    _ => {}
                }
            }
        }

        // Convert deprecated tailwind config to Tailwind v4 CSS format
        if let Some(ref tailwind) = resolved.spec.tailwind {
            convert_tailwind_to_mutations(tailwind, &mut mutations);
        }
    }

    mutations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::models::{RegistryFile, RegistryItem, RegistryItemType};

    fn make_registry_file(path: &str, file_type: &str) -> RegistryFile {
        RegistryFile {
            path: path.to_string(),
            content: String::new(),
            target: None,
            file_type: Some(file_type.to_string()),
        }
    }

    fn make_resolved(
        name: &str,
        registry: Option<&str>,
        files: Vec<RegistryFile>,
    ) -> ResolvedComponent {
        ResolvedComponent {
            name: name.to_string(),
            spec: RegistryItem {
                name: name.to_string(),
                title: None,
                description: None,
                item_type: RegistryItemType::Ui,
                files,
                dependencies: vec![],
                registry_dependencies: vec![],
                css_vars: None,
                css: None,
                tailwind: None,
                docs: None,
                categories: vec![],
                meta: None,
            },
            registry: registry.map(|s| s.to_string()),
            warnings: vec![],
        }
    }

    #[test]
    fn test_rewrite_flattened_paths_component() {
        let mut map = HashMap::new();
        map.insert(
            "@/components/animate-ui/icons/icon".to_string(),
            "@/components/animate-ui/icons-icon".to_string(),
        );
        let content = r#"import { Icon } from "@/components/animate-ui/icons/icon""#;
        let result = rewrite_flattened_paths(content, &map);
        assert_eq!(
            result,
            r#"import { Icon } from "@/components/animate-ui/icons-icon""#
        );
    }

    #[test]
    fn test_rewrite_flattened_paths_hook() {
        let mut map = HashMap::new();
        map.insert(
            "@/hooks/use-is-in-view".to_string(),
            "@/hooks/hooks-use-is-in-view".to_string(),
        );
        let content = r#"import { useIsInView } from "@/hooks/use-is-in-view""#;
        let result = rewrite_flattened_paths(content, &map);
        assert_eq!(
            result,
            r#"import { useIsInView } from "@/hooks/hooks-use-is-in-view""#
        );
    }

    #[test]
    fn test_rewrite_flattened_paths_nested() {
        let mut map = HashMap::new();
        map.insert(
            "@/components/animate-ui/primitives/animate/slot".to_string(),
            "@/components/animate-ui/primitives-animate-slot".to_string(),
        );
        let content = r#"import { Slot } from "@/components/animate-ui/primitives/animate/slot""#;
        let result = rewrite_flattened_paths(content, &map);
        assert_eq!(
            result,
            r#"import { Slot } from "@/components/animate-ui/primitives-animate-slot""#
        );
    }

    #[test]
    fn test_rewrite_flattened_paths_no_partial_match() {
        let mut map = HashMap::new();
        map.insert(
            "@/hooks/icon-button".to_string(),
            "@/hooks/hooks-icon-button".to_string(),
        );
        // "@/hooks/icon" must NOT be rewritten — only icon-button is in the map
        let content = r#"import { Icon } from "@/hooks/icon""#;
        let result = rewrite_flattened_paths(content, &map);
        assert_eq!(result, r#"import { Icon } from "@/hooks/icon""#);
    }

    #[test]
    fn test_rewrite_flattened_paths_empty_map() {
        let map = HashMap::new();
        let content = r#"import { Foo } from "@/components/ui/foo""#;
        let result = rewrite_flattened_paths(content, &map);
        assert_eq!(result, content);
    }

    #[test]
    fn test_original_registry_import_path_tsx() {
        assert_eq!(
            original_registry_import_path("components/animate-ui/icons/icon.tsx"),
            "@/components/animate-ui/icons/icon"
        );
    }

    #[test]
    fn test_original_registry_import_path_ts() {
        assert_eq!(
            original_registry_import_path("hooks/use-is-in-view.ts"),
            "@/hooks/use-is-in-view"
        );
    }

    #[test]
    fn test_build_path_map() {
        let components = vec![
            make_resolved(
                "icons-icon",
                Some("@animate-ui"),
                vec![make_registry_file(
                    "components/animate-ui/icons/icon.tsx",
                    "registry:ui",
                )],
            ),
            make_resolved(
                "hooks-use-is-in-view",
                Some("@animate-ui"),
                vec![make_registry_file(
                    "hooks/use-is-in-view.ts",
                    "registry:hook",
                )],
            ),
        ];
        let map = build_path_map(&components);
        assert_eq!(
            map.get("@/components/animate-ui/icons/icon"),
            Some(&"@/components/animate-ui/icons-icon".to_string())
        );
        assert_eq!(
            map.get("@/hooks/use-is-in-view"),
            Some(&"@/hooks/hooks-use-is-in-view".to_string())
        );
    }

    #[test]
    fn test_detect_external_imports() {
        let files = vec![PlannedFile {
            relative_path: PathBuf::from("ui/message.tsx"),
            absolute_path: PathBuf::from("/tmp/test/src/components/ui/message.tsx"),
            content: r#"
import { useState } from "react";
import { streamText } from "ai";
import { Markdown } from "streamdown/react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { localHelper } from "./helper";
import { otherHelper } from "../shared/utils";
import "highlight.js/styles/github.css";
import { SomeScoped } from "@radix-ui/react-dialog";
"#
            .to_string(),
            source_component: "message".to_string(),
        }];

        let result = detect_external_imports(&files);

        assert!(result.contains(&"ai".to_string()));
        assert!(result.contains(&"streamdown".to_string()));
        assert!(result.contains(&"highlight.js".to_string()));
        assert!(result.contains(&"@radix-ui/react-dialog".to_string()));
        // Should NOT contain:
        assert!(!result.contains(&"react".to_string()));
        assert!(!result.iter().any(|s| s.starts_with("@/")));
        assert!(!result.iter().any(|s| s.starts_with('.')));
    }

    #[test]
    fn test_detect_external_imports_skips_non_ts_files() {
        let files = vec![PlannedFile {
            relative_path: PathBuf::from("readme.md"),
            absolute_path: PathBuf::from("/tmp/test/readme.md"),
            content: r#"import { foo } from "some-package""#.to_string(),
            source_component: "readme".to_string(),
        }];

        let result = detect_external_imports(&files);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_import_specifiers() {
        let content = r#"
import { useState } from "react";
import { streamText } from 'ai';
import "side-effect-pkg";
import type { Foo } from "type-pkg";
const x = require("not-matched");
"#;
        let specs = extract_import_specifiers(content);
        assert!(specs.contains(&"react".to_string()));
        assert!(specs.contains(&"ai".to_string()));
        assert!(specs.contains(&"side-effect-pkg".to_string()));
        assert!(specs.contains(&"type-pkg".to_string()));
        // require() is not matched
        assert!(!specs.contains(&"not-matched".to_string()));
    }

    #[test]
    fn test_integration_issue_88() {
        // Simulate icons-icon.tsx importing from icons-arrow-right using the original nested path
        let components = vec![
            make_resolved(
                "icons-arrow-right",
                Some("@animate-ui"),
                vec![make_registry_file(
                    "components/animate-ui/icons/arrow-right.tsx",
                    "registry:ui",
                )],
            ),
            make_resolved(
                "icons-icon",
                Some("@animate-ui"),
                vec![make_registry_file(
                    "components/animate-ui/icons/icon.tsx",
                    "registry:ui",
                )],
            ),
        ];
        let map = build_path_map(&components);

        let content = concat!(
            r#"import { ArrowRight } from "@/components/animate-ui/icons/arrow-right""#,
            "\n",
            r#"import { Icon } from "@/components/animate-ui/icons/icon""#,
        );
        let result = rewrite_flattened_paths(content, &map);

        assert!(result.contains("@/components/animate-ui/icons-arrow-right"));
        assert!(!result.contains("@/components/animate-ui/icons/arrow-right"));
        assert!(result.contains("@/components/animate-ui/icons-icon"));
        assert!(!result.contains("@/components/animate-ui/icons/icon\""));
    }
}
