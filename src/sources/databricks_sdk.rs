//! Databricks SDK documentation retrieval and parsing.
//!
//! This module handles downloading, caching, and parsing SDK documentation
//! from GitHub. The actual indexing and search is handled by `search::docs_index`.

use crate::common::Timer;
use rayon::prelude::*;
use serde::Deserialize;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const GITHUB_REPO: &str = "databricks/databricks-sdk-py";

/// SDK documentation source enum
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SDKSource {
    #[serde(rename = "databricks-sdk-python")]
    DatabricksSdkPython,
}

/// Parsed documentation file with metadata
pub struct ParsedDocFile {
    pub relative_path: String,
    pub text: String,
    pub service: String,
    pub entity: String,
    pub operation: String,
    pub symbols: String,
}

/// Get cache path for SDK documentation
fn get_cache_path(version: &str) -> Result<PathBuf, String> {
    Ok(dirs::home_dir()
        .ok_or("Could not determine home directory")?
        .join(".apx")
        .join("cache")
        .join("databricks-sdk")
        .join(version))
}

/// Check if docs are cached for this version
fn is_cached(version: &str) -> bool {
    let Ok(cache_path) = get_cache_path(version) else {
        return false;
    };
    let docs_path = cache_path.join("docs");
    let exists = docs_path.exists();
    let has_files = docs_path
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    tracing::debug!(
        "is_cached: version={}, cache_path={:?}, docs_path={:?}, exists={}, has_files={}",
        version,
        cache_path,
        docs_path,
        exists,
        has_files
    );
    exists && has_files
}

/// Get GitHub zipball URL for a specific version
fn get_github_zipball_url(version: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/archive/refs/tags/v{version}.zip")
}

/// Download and extract SDK repository
pub async fn download_and_extract_sdk(version: &str) -> Result<PathBuf, String> {
    let download_timer = Timer::start(format!("download_sdk_v{version}"));
    let cache_path = get_cache_path(version)?;
    let docs_path = cache_path.join("docs");

    if is_cached(version) {
        tracing::debug!("SDK docs already cached at {:?}", docs_path);
        download_timer.lap("Using cached SDK docs");
        return Ok(docs_path);
    }

    tracing::info!("Downloading Databricks SDK v{} from GitHub", version);
    let url = get_github_zipball_url(version);

    let http_timer = Timer::start("http_download");
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to download SDK: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to download SDK: HTTP {}",
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;
    http_timer.lap(&format!("Downloaded {} MB", bytes.len() / 1_048_576));

    // Extract ZIP
    let extract_timer = Timer::start("zip_extraction");
    fs::create_dir_all(&cache_path)
        .map_err(|e| format!("Failed to create cache directory: {e}"))?;

    let cursor = Cursor::new(bytes.to_vec());
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to read ZIP archive: {e}"))?;
    tracing::debug!(
        "download_and_extract_sdk: ZIP archive has {} files",
        archive.len()
    );

    // Find root folder name
    let root_folder = if !archive.is_empty() {
        let first_file = archive
            .by_index(0)
            .map_err(|e| format!("Failed to read first file: {e}"))?;
        let name = first_file.name();
        name.split('/').next().unwrap_or("").to_string()
    } else {
        return Err("Empty ZIP archive".to_string());
    };

    // Extract docs/ folder
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read file at index {i}: {e}"))?;

        let file_path = file.name().to_string();

        // Only extract docs/ folder
        if !file_path.starts_with(&format!("{root_folder}/docs/")) {
            continue;
        }

        let relative_path = file_path
            .strip_prefix(&format!("{root_folder}/"))
            .ok_or_else(|| format!("Failed to strip prefix from path: {file_path}"))?;
        let target_path = cache_path.join(relative_path);

        if file.is_dir() {
            fs::create_dir_all(&target_path)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        } else {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {e}"))?;
            }

            let mut outfile = fs::File::create(&target_path)
                .map_err(|e| format!("Failed to create file: {e}"))?;

            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("Failed to write file: {e}"))?;
        }
    }

    extract_timer.lap(&format!("Extracted {} files", archive.len()));
    download_timer.finish();
    Ok(docs_path)
}

/// Extract method name from signature like "create(spark_version: str, ...)" -> "create"
fn extract_method_name(signature: &str) -> Option<&str> {
    let name_part = signature.split('(').next()?;
    // If it has a dot (e.g., "Class.method"), get the last part
    Some(name_part.split('.').next_back().unwrap_or(name_part))
}

/// Extract service name from class name (e.g., "ClustersAPI" -> "clusters")
fn extract_service_from_class(class_name: &str) -> Option<String> {
    let name = class_name.trim_end_matches("API").trim_end_matches("Ext");

    if name.is_empty() {
        None
    } else {
        Some(name.to_lowercase())
    }
}

/// Parsed RST directive result
struct ParsedDirective {
    /// Text fragments to include in searchable text
    text_fragments: Vec<String>,
    /// Entity name (class)
    entity: Option<String>,
    /// Operation name (method)
    operation: Option<String>,
    /// Service name extracted from class
    service: Option<String>,
}

/// Parse a single RST directive line, returning extracted content
fn parse_rst_directive(directive_content: &str) -> Option<ParsedDirective> {
    let double_colon_pos = directive_content.find("::")?;
    let directive_type = &directive_content[..double_colon_pos];
    let directive_value = directive_content[double_colon_pos + 2..].trim();

    if directive_value.is_empty() && directive_type != "code-block" {
        return None;
    }

    let mut result = ParsedDirective {
        text_fragments: Vec::new(),
        entity: None,
        operation: None,
        service: None,
    };

    match directive_type {
        t if t.starts_with("py:class") => {
            result.text_fragments.push(directive_value.to_string());
            result.entity = Some(directive_value.to_string());
            if let Some(svc) = extract_service_from_class(directive_value) {
                result.text_fragments.push(svc.clone());
                result.service = Some(svc);
            }
        }
        t if t.starts_with("py:method") => {
            result.text_fragments.push(directive_value.to_string());
            if let Some(method_name) = extract_method_name(directive_value) {
                result.text_fragments.push(method_name.to_string());
                result.operation = Some(method_name.to_string());
            }
        }
        t if t.starts_with("py:attribute") => {
            result
                .text_fragments
                .push(format!("attribute {directive_value}"));
        }
        "autoclass" => {
            result
                .text_fragments
                .push(format!("class {directive_value}"));
        }
        t if t.starts_with("py:currentmodule") || t == "currentmodule" => {
            result
                .text_fragments
                .push(format!("module {directive_value}"));
        }
        "code-block" => {
            result.text_fragments.push("code example".to_string());
        }
        _ => return None,
    }

    Some(result)
}

/// Directive-aware RST to text converter
/// Returns (text, entity, operation, service, symbols)
fn parse_rst_content(rst_content: &str) -> (String, String, String, String, Vec<String>) {
    let mut output = Vec::new();
    let mut in_code_block = false;

    // Metadata collected from directives
    let mut entity = String::new();
    let mut operation = String::new();
    let mut service = String::new();
    let mut symbols = Vec::new();

    for line in rst_content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Handle code blocks
        if in_code_block {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_code_block = false;
                // Fall through to process this line
            } else {
                output.push(trimmed.to_string());
                continue;
            }
        }

        // Skip RST heading underlines (===, ---, ~~~, etc.)
        if trimmed
            .chars()
            .all(|c| matches!(c, '=' | '-' | '~' | '^' | '*'))
        {
            continue;
        }

        // Process RST directives (lines starting with ".. ")
        if let Some(directive_content) = trimmed.strip_prefix(".. ") {
            if directive_content.starts_with("code-block::") {
                in_code_block = true;
                output.push("code example".to_string());
            } else if let Some(parsed) = parse_rst_directive(directive_content) {
                output.extend(parsed.text_fragments);

                // Collect metadata (keep first entity/service, collect all operations)
                if entity.is_empty() {
                    if let Some(e) = parsed.entity {
                        entity = e.clone();
                        symbols.push(e);
                    }
                }
                if let Some(s) = parsed.service {
                    if service.is_empty() {
                        service = s.clone();
                    }
                    symbols.push(s);
                }
                if let Some(op) = parsed.operation {
                    operation = op.clone(); // Keep last operation (methods come after class)
                    symbols.push(op);
                }
            }
            continue;
        }

        // Process field directives (:param:, :returns:, :value:, etc.)
        if let Some(stripped) = trimmed.strip_prefix(':') {
            if let Some(colon_end) = stripped.find(':') {
                let field_name = &stripped[..colon_end];
                let field_value = trimmed.get(colon_end + 2..).map(|s| s.trim()).unwrap_or("");

                let text = match field_name {
                    f if f.starts_with("param ") => {
                        let param_name = f.split_whitespace().nth(1).unwrap_or("");
                        if field_value.is_empty() {
                            format!("param {param_name}")
                        } else {
                            format!("param {param_name} {field_value}")
                        }
                    }
                    f if f.starts_with("type ") => {
                        let type_name = f.split_whitespace().nth(1).unwrap_or("");
                        if field_value.is_empty() {
                            continue;
                        }
                        format!("type {type_name} {field_value}")
                    }
                    "returns" if !field_value.is_empty() => format!("returns {field_value}"),
                    "value" if !field_value.is_empty() => format!("value {field_value}"),
                    "members" | "undoc-members" => continue, // Skip directive options
                    _ => continue,
                };
                output.push(text);
            }
            continue;
        }

        // Preserve markdown-style links: [Link Text]: https://url
        if trimmed.starts_with('[') && trimmed.contains("]:") {
            output.push(trimmed.to_string());
            continue;
        }

        // Regular prose content (including heading text)
        output.push(trimmed.to_string());
    }

    (output.join(" "), entity, operation, service, symbols)
}

/// Extract metadata from file path and RST content
fn extract_metadata(
    file_path: &str,
    rst_content: &str,
) -> (String, String, String, String, String) {
    let (text, entity, operation, mut service, mut symbols) = parse_rst_content(rst_content);

    // Fallback: extract service from file path if not found in content
    if service.is_empty() {
        if let Some(stem) = Path::new(file_path).file_stem() {
            service = stem.to_string_lossy().to_lowercase();
        }
    }

    // Add service to symbols for matching
    if !service.is_empty() && !symbols.contains(&service) {
        symbols.push(service.clone());
    }

    (text, service, entity, operation, symbols.join(" "))
}

/// Simple markdown to text converter
fn md_to_text(md_content: &str) -> String {
    let mut output = Vec::new();
    let mut in_code_block = false;

    for line in md_content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            output.push(trimmed.to_string());
            continue;
        }

        // Remove markdown heading markers but keep text
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                output.push(heading.to_string());
            }
            continue;
        }

        output.push(trimmed.to_string());
    }

    output.join(" ")
}

/// Load RST files from a directory (recursive), skipping index.rst
fn load_rst_from_dir(
    dir_path: &Path,
    docs_path: &Path,
    files: &mut Vec<ParsedDocFile>,
) -> Result<(), String> {
    if !dir_path.exists() {
        return Ok(());
    }

    // Collect all valid RST file paths first
    let rst_paths: Vec<PathBuf> = walkdir::WalkDir::new(dir_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let path = entry.path();

            // Only .rst files, skip index.rst
            if path.extension().and_then(|s| s.to_str()) != Some("rst") {
                return None;
            }
            if path.file_stem().and_then(|s| s.to_str()) == Some("index") {
                return None;
            }

            Some(path.to_path_buf())
        })
        .collect();

    // Process files in parallel
    let parsed_files: Vec<ParsedDocFile> = rst_paths
        .par_iter()
        .filter_map(|path| {
            let content = fs::read_to_string(path).ok()?;

            let relative_path = path
                .strip_prefix(docs_path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            let (text, service, entity, operation, symbols) =
                extract_metadata(&relative_path, &content);

            if text.is_empty() {
                return None;
            }

            Some(ParsedDocFile {
                relative_path,
                text,
                service,
                entity,
                operation,
                symbols,
            })
        })
        .collect();

    files.extend(parsed_files);

    Ok(())
}

/// Load all documentation files (RST from workspace/dbdataclasses, MD from root)
pub fn load_doc_files(docs_path: &Path) -> Result<Vec<ParsedDocFile>, String> {
    let load_timer = Timer::start("load_all_doc_files");
    let mut files = Vec::new();

    // Load RST from workspace/ and dbdataclasses/
    let rst_timer = Timer::start("load_rst_files");
    load_rst_from_dir(&docs_path.join("workspace"), docs_path, &mut files)?;
    let workspace_count = files.len();
    load_rst_from_dir(&docs_path.join("dbdataclasses"), docs_path, &mut files)?;
    let dbdataclasses_count = files.len() - workspace_count;
    rst_timer.lap(&format!(
        "Loaded {} RST files (workspace: {}, dbdataclasses: {})",
        workspace_count + dbdataclasses_count,
        workspace_count,
        dbdataclasses_count
    ));

    // Load markdown files from docs root - collect paths first
    let md_paths: Vec<PathBuf> = fs::read_dir(docs_path)
        .map_err(|e| format!("Failed to read docs directory: {e}"))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("md") {
                return None;
            }

            Some(path)
        })
        .collect();

    // Process markdown files in parallel
    let md_files: Vec<ParsedDocFile> = md_paths
        .par_iter()
        .filter_map(|path| {
            let content = fs::read_to_string(path).ok()?;
            let text = md_to_text(&content);

            if text.is_empty() {
                return None;
            }

            let relative_path = path
                .strip_prefix(docs_path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();

            Some(ParsedDocFile {
                relative_path,
                text,
                service: file_stem.clone(),
                entity: String::new(),
                operation: String::new(),
                symbols: format!("{file_stem} guide documentation"),
            })
        })
        .collect();

    files.extend(md_files);

    if files.is_empty() {
        return Err("No documentation files found".to_string());
    }

    load_timer.finish();
    Ok(files)
}
