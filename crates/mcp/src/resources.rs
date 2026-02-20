use crate::info_content::APX_INFO_CONTENT;
use crate::tools::project::parse_openapi_operations;
use crate::validation::validate_app_path;
use rmcp::model::*;
use serde::Serialize;

pub fn list_resources() -> Vec<Resource> {
    let mut raw = RawResource::new("apx://info", "apx-info".to_string());
    raw.description = Some("Information about apx toolkit".to_string());
    raw.mime_type = Some("text/plain".to_string());
    vec![raw.no_annotation()]
}

pub fn list_resource_templates() -> Vec<ResourceTemplate> {
    let raw = RawResourceTemplate {
        uri_template: "apx://project/{app_path}".to_string(),
        name: "project-context".to_string(),
        title: Some("Project Context".to_string()),
        description: Some(
            "Comprehensive project context including routes, schemas, and installed UI components."
                .to_string(),
        ),
        mime_type: Some("application/json".to_string()),
        icons: None,
    };
    vec![raw.no_annotation()]
}

pub fn read_resource(uri: &str) -> Result<ReadResourceResult, String> {
    match uri {
        "apx://info" => Ok(ReadResourceResult {
            contents: vec![ResourceContents::text(APX_INFO_CONTENT, uri)],
        }),
        _ => Err(format!("Resource not found: {uri}")),
    }
}

#[derive(Serialize)]
struct RouteSummary {
    id: String,
    method: String,
    path: String,
    hook_name: String,
}

#[derive(Serialize)]
struct ProjectContext {
    app_name: String,
    app_slug: String,
    api_prefix: String,
    has_ui: bool,
    routes: Vec<RouteSummary>,
    ui_components: Vec<String>,
    python_dependencies: Vec<String>,
    backend_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sdk_version: Option<String>,
    configured_registries: Vec<String>,
}

pub async fn read_project_resource(app_path: &str) -> Result<ReadResourceResult, String> {
    let path = validate_app_path(app_path)?;

    use apx_core::common::{read_project_metadata, read_python_dependencies};
    use apx_core::interop::get_databricks_sdk_version_for_project;

    let metadata = read_project_metadata(&path)?;

    // Best-effort: try to generate OpenAPI and parse routes
    let routes: Vec<RouteSummary> = try_parse_routes(&path, &metadata).await.unwrap_or_default();

    // Scan for installed UI components
    let ui_components = scan_ui_components(&path);

    // Read Python dependencies from pyproject.toml
    let python_dependencies = read_python_dependencies(&path);

    // Scan backend .py files (paths relative to project root)
    let backend_files = scan_backend_files(&path, &metadata.app_slug);

    // Best-effort: detect installed SDK version
    let sdk_version = get_databricks_sdk_version_for_project(&path).unwrap_or(None);

    // Configured UI component registries
    let configured_registries = metadata
        .ui_registries
        .as_ref()
        .map(|r| r.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    let has_ui = metadata.ui_root.is_some();
    let context = ProjectContext {
        app_name: metadata.app_name,
        app_slug: metadata.app_slug,
        api_prefix: metadata.api_prefix,
        has_ui,
        routes,
        ui_components,
        python_dependencies,
        backend_files,
        sdk_version,
        configured_registries,
    };

    let json = serde_json::to_string_pretty(&context)
        .map_err(|e| format!("Failed to serialize project context: {e}"))?;

    let uri = format!("apx://project/{app_path}");
    Ok(ReadResourceResult {
        contents: vec![ResourceContents::text(json, uri)],
    })
}

async fn try_parse_routes(
    path: &std::path::Path,
    metadata: &apx_core::common::ProjectMetadata,
) -> Result<Vec<RouteSummary>, String> {
    use apx_core::interop::generate_openapi_spec;

    let (openapi_content, _) =
        generate_openapi_spec(path, &metadata.app_entrypoint, &metadata.app_slug).await?;

    let openapi: serde_json::Value =
        serde_json::from_str(&openapi_content).map_err(|e| format!("Parse error: {e}"))?;

    let route_infos = parse_openapi_operations(&openapi)?;

    Ok(route_infos
        .into_iter()
        .map(|r| RouteSummary {
            id: r.id,
            method: r.method,
            path: r.path,
            hook_name: r.hook_name,
        })
        .collect())
}

fn scan_ui_components(project_root: &std::path::Path) -> Vec<String> {
    let ui_dir = project_root.join("ui/src/components/ui");
    let Ok(entries) = std::fs::read_dir(&ui_dir) else {
        return Vec::new();
    };

    let mut components: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Include .tsx files, strip extension for component name
            if name.ends_with(".tsx") {
                Some(name.trim_end_matches(".tsx").to_string())
            } else {
                None
            }
        })
        .collect();

    components.sort();
    components
}

/// Scan for Python files in the backend source directory.
/// Returns paths relative to project root (e.g. "src/my_app/backend/router.py").
fn scan_backend_files(project_root: &std::path::Path, app_slug: &str) -> Vec<String> {
    let rel_prefix = std::path::Path::new("src").join(app_slug).join("backend");
    let backend_dir = project_root.join(&rel_prefix);
    let Ok(entries) = std::fs::read_dir(&backend_dir) else {
        return Vec::new();
    };

    let mut files: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".py") && !name.starts_with("__") {
                Some(rel_prefix.join(&name).to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();

    files.sort();
    files
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn list_resource_templates_returns_project() {
        let templates = list_resource_templates();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].raw.uri_template, "apx://project/{app_path}");
        assert_eq!(templates[0].raw.name, "project-context");
    }

    #[test]
    fn read_project_resource_rejects_relative_path() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(read_project_resource("relative/path"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("absolute path"), "got: {err}");
    }

    #[test]
    fn read_project_resource_rejects_nonexistent() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(read_project_resource("/tmp/__apx_test_nonexistent_proj__"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn scan_backend_files_returns_relative_py_paths() {
        let tmp = std::env::temp_dir().join("apx_test_scan_backend");
        let _ = std::fs::remove_dir_all(&tmp);
        let backend_dir = tmp.join("src").join("test_app").join("backend");
        std::fs::create_dir_all(&backend_dir).unwrap();
        std::fs::write(backend_dir.join("app.py"), "").unwrap();
        std::fs::write(backend_dir.join("router.py"), "").unwrap();
        std::fs::write(backend_dir.join("__init__.py"), "").unwrap();
        let files = scan_backend_files(&tmp, "test_app");
        assert_eq!(
            files,
            vec![
                "src/test_app/backend/app.py",
                "src/test_app/backend/router.py"
            ]
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn scan_backend_files_empty_on_missing_dir() {
        let tmp = std::env::temp_dir().join("apx_test_scan_backend_missing");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let files = scan_backend_files(&tmp, "nonexistent");
        assert!(files.is_empty());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
