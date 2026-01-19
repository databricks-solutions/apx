pub mod add;
pub mod utils;

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, trace};

/// Default shadcn/ui registry item template.
///
/// IMPORTANT: /r/{name}.json is 404.
/// The working endpoints are style-scoped:
///   https://ui.shadcn.com/r/styles/{style}/{name}.json
/// Example:
///   https://ui.shadcn.com/r/styles/new-york/button.json
pub const SHADCN_REGISTRY_ITEM_TEMPLATE: &str = "https://ui.shadcn.com/r/styles/{style}/{name}.json";

/// Default style if components.json doesn't specify one.
pub const DEFAULT_STYLE: &str = "new-york";

/// Subset of components.json we actually need (plus "style")
#[derive(Debug, Deserialize)]
pub struct ComponentsJson {
    #[serde(default)]
    pub style: Option<String>,

    #[serde(default)]
    pub aliases: HashMap<String, String>,

    /// Registries can be:
    ///   - string template: "https://example.com/r/{name}.json"
    ///   - advanced object: { "url": "...", "headers": {...}, "params": {...} }
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RegistryConfig {
    Template(String),
    Advanced(RegistryAdvanced),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAdvanced {
    pub url: String,

    #[serde(default)]
    pub headers: HashMap<String, String>,

    #[serde(default)]
    pub params: HashMap<String, String>,
}

/// Component JSON (registry item)
#[derive(Debug, Deserialize)]
pub struct ComponentSpec {
    pub name: String,
    pub files: Vec<ComponentFile>,

    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ComponentFile {
    pub path: String,
    pub content: String,

    /// Some registry items include "target" (often empty). Keep it optional.
    #[allow(dead_code)]
    #[serde(default)]
    pub target: Option<String>,

    #[allow(dead_code)]
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
}

/// Request resolved from a registry definition (URL + optional headers/params).
#[derive(Debug, Clone)]
pub struct ResolvedRequest {
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct TsConfig {
    #[serde(rename = "compilerOptions")]
    compiler_options: Option<TsCompilerOptions>,
}

#[derive(Debug, Deserialize)]
struct TsCompilerOptions {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,

    paths: Option<HashMap<String, Vec<String>>>,
}

pub fn load_components_json(app_dir: &Path) -> Result<ComponentsJson, String> {
    let path = app_dir.join("components.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read components.json: {e}"))?;

    serde_json::from_str(&text).map_err(|e| format!("Invalid components.json: {e}"))
}

/// Resolve "@/components/ui" → filesystem path using tsconfig.json (JSONC allowed).
pub fn resolve_ui_base_dir(app_dir: &Path, cfg: &ComponentsJson) -> Result<PathBuf, String> {
    let ui_alias = cfg
        .aliases
        .get("ui")
        .ok_or("components.json missing aliases.ui")?;

    let tsconfig_path = app_dir.join("tsconfig.json");
    debug!(path = %tsconfig_path.display(), "Reading tsconfig.json");

    let raw = std::fs::read_to_string(&tsconfig_path)
        .map_err(|e| format!("Failed to read tsconfig.json: {e}"))?;

    trace!(length = raw.len(), "Raw tsconfig.json loaded");
    trace!("Raw tsconfig.json first 30 lines:");
    for (i, line) in raw.lines().take(30).enumerate() {
        trace!(line_num = i + 1, line = %line, "Raw tsconfig line");
    }

    let stripped = crate::cli::components::utils::strip_jsonc_comments(&raw);

    trace!(length = stripped.len(), "Stripped tsconfig.json after comment removal");
    trace!("Stripped tsconfig.json first 30 lines:");
    for (i, line) in stripped.lines().take(30).enumerate() {
        trace!(line_num = i + 1, line = %line, "Stripped tsconfig line");
    }

    let tsconfig: TsConfig = serde_json::from_str(&stripped).map_err(|e| {
        debug!(error = %e, "JSON parse error");
        debug!(error_details = ?e, "Full error details");
        format!("Invalid tsconfig.json: {e}")
    })?;

    let compiler = tsconfig
        .compiler_options
        .ok_or("tsconfig.json missing compilerOptions")?;

    let base_url = compiler.base_url.unwrap_or_else(|| ".".to_string());
    let base_url = Path::new(&base_url);

    let paths = compiler
        .paths
        .ok_or("tsconfig.json missing compilerOptions.paths")?;

    for (key, targets) in paths {
        if let Some(prefix) = key.strip_suffix('*') {
            if ui_alias.starts_with(prefix) {
                let remainder = ui_alias.trim_start_matches(prefix);

                let target = targets.first().ok_or("tsconfig paths entry is empty")?;

                let target_base = target.strip_suffix('*').unwrap_or(target);

                return Ok(app_dir.join(base_url).join(target_base).join(remainder));
            }
        }
    }

    Err(format!(
        "Failed to resolve alias `{ui_alias}` via tsconfig.json paths"
    ))
}

/// Resolve a component spec request.
///
/// Behavior:
/// - If `component` is a full URL: use it directly.
/// - If `registry` is None: use shadcn default template with {style}.
/// - Else: look up registry in components.json registries and resolve {name}, {style},
///   and ${ENV_VAR} in headers/params.
pub fn resolve_component_request(
    cfg: &ComponentsJson,
    registry: Option<&str>,
    component: &str,
) -> Result<ResolvedRequest, String> {
    // 1) Explicit URL provided
    if component.starts_with("http://") || component.starts_with("https://") {
        return Ok(ResolvedRequest {
            url: component.to_string(),
            headers: HashMap::new(),
        });
    }

    let style = cfg.style.as_deref().unwrap_or(DEFAULT_STYLE);

    // 2) Default registry: shadcn/ui
    if registry.is_none() {
        let url = SHADCN_REGISTRY_ITEM_TEMPLATE
            .replace("{style}", style)
            .replace("{name}", component);

        debug!(
            component = component,
            style = style,
            url = url.as_str(),
            "Resolving via default shadcn registry"
        );

        return Ok(ResolvedRequest {
            url,
            headers: HashMap::new(),
        });
    }

    // 3) Named registry from components.json
    let registry_name = registry.unwrap();
    let reg = cfg
        .registries
        .get(registry_name)
        .ok_or_else(|| format!("Unknown registry: {registry_name}"))?
        .clone();

    match reg {
        RegistryConfig::Template(tpl) => {
            let url = apply_placeholders(&tpl, component, style)?;
            debug!(
                registry = registry_name,
                component = component,
                style = style,
                url = url.as_str(),
                "Resolving via template registry"
            );

            Ok(ResolvedRequest {
                url,
                headers: HashMap::new(),
            })
        }
        RegistryConfig::Advanced(adv) => {
            let mut url = apply_placeholders(&adv.url, component, style)?;

            // Params become query string entries
            if !adv.params.is_empty() {
                let mut first = !url.contains('?');
                for (k, v) in adv.params {
                    let k = expand_env(&k);
                    let v = expand_env(&v);

                    let sep = if first { '?' } else { '&' };
                    first = false;

                    // Minimal escaping; for full URL encoding use url crate.
                    url.push(sep);
                    url.push_str(&url_encode_component(&k));
                    url.push('=');
                    url.push_str(&url_encode_component(&v));
                }
            }

            // Headers (env expanded)
            let mut headers = HashMap::new();
            for (k, v) in adv.headers {
                headers.insert(expand_env(&k), expand_env(&v));
            }

            debug!(
                registry = registry_name,
                component = component,
                style = style,
                url = url.as_str(),
                headers_len = headers.len(),
                "Resolving via advanced registry"
            );

            Ok(ResolvedRequest { url, headers })
        }
    }
}

/// Fetch component spec, applying headers from resolved request.
pub async fn fetch_component(
    client: &reqwest::Client,
    req: &ResolvedRequest,
) -> Result<ComponentSpec, String> {
    let mut rb = client.get(&req.url);
    for (k, v) in &req.headers {
        rb = rb.header(k, v);
    }

    rb.send()
        .await
        .map_err(|e| format!("Failed to fetch component: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Registry returned error: {e}"))?
        .json::<ComponentSpec>()
        .await
        .map_err(|e| format!("Invalid component spec: {e}"))
}

/// Writes component files into ui_base_dir.
///
/// NOTE: shadcn registry item file paths often include a leading "ui/" or
/// "registry/<style>/ui/". Since ui_base_dir already points at the ui folder,
/// we normalize to avoid nested "ui/ui/...".
pub fn write_component_files(
    ui_base_dir: &Path,
    spec: &ComponentSpec,
    force: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut written = vec![];

    for file in &spec.files {
        let rel = normalize_ui_file_path(&file.path);
        let target = ui_base_dir.join(rel);

        if target.exists() && !force {
            return Err(format!(
                "File already exists (use --force): {}",
                target.display()
            ));
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

        std::fs::write(&target, &file.content)
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;

        written.push(target);
    }

    Ok(written)
}

// ---------------------- helpers ----------------------

fn apply_placeholders(template: &str, name: &str, style: &str) -> Result<String, String> {
    // If it's missing {name}, allow appending "/{name}.json" for convenience.
    let mut url = template.to_string();

    // Support {style} placeholder (documented).
    url = url.replace("{style}", style);

    if url.contains("{name}") {
        url = url.replace("{name}", name);
    } else if url.contains("{name}") == false && url.ends_with('/') {
        url.push_str(name);
    } else if url.contains("{name}") == false && !url.ends_with(".json") {
        // Heuristic: append "/<name>.json"
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str(name);
        url.push_str(".json");
    }

    Ok(url)
}

/// Expand ${VAR_NAME} from process environment.
/// Undefined vars are replaced with empty string (safe default).
fn expand_env(s: &str) -> String {
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
            let val = std::env::var(key).unwrap_or_default();
            out.push_str(&val);
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    out
}

/// Minimal URL component encoding (enough for query params).
fn url_encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Normalize shadcn registry file paths so they land under ui_base_dir correctly.
fn normalize_ui_file_path(path: &str) -> &str {
    // common patterns:
    // - "ui/button.tsx"
    // - "registry/new-york-v4/ui/button.tsx"
    if let Some(idx) = path.rfind("/ui/") {
        return &path[idx + 4..]; // after "/ui/"
    }
    if let Some(stripped) = path.strip_prefix("ui/") {
        return stripped;
    }
    path
}
