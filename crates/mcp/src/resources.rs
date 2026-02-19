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
}

pub async fn read_project_resource(app_path: &str) -> Result<ReadResourceResult, String> {
    let path = validate_app_path(app_path)?;

    use apx_core::common::read_project_metadata;

    let metadata = read_project_metadata(&path)?;

    // Best-effort: try to generate OpenAPI and parse routes
    let routes: Vec<RouteSummary> = try_parse_routes(&path, &metadata).await.unwrap_or_default();

    // Scan for installed UI components
    let ui_components = scan_ui_components(&path);

    let has_ui = metadata.ui_root.is_some();
    let context = ProjectContext {
        app_name: metadata.app_name,
        app_slug: metadata.app_slug,
        api_prefix: metadata.api_prefix,
        has_ui,
        routes,
        ui_components,
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
}
