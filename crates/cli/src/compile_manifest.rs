//! Compile a framework manifest from a FastAPI app.
//!
//! Orchestrates the Python extraction script, deserializes the JSON output
//! into an [`AppManifest`], runs structural validation, and writes the
//! result to `.build/manifest.json`.

use apx_framework::manifest;
use apx_framework::route::AppManifest;
use std::path::Path;

use apx_core::external::uv::Uv;

/// Default manifest filename inside the build directory.
const MANIFEST_FILENAME: &str = "manifest.json";

/// Compile and write a manifest for the given app module.
///
/// 1. Run Python extraction script via `uv run`
/// 2. Deserialize JSON into [`AppManifest`]
/// 3. Run Rust-side structural validation
/// 4. Write to `<build_dir>/manifest.json`
pub async fn compile_manifest(
    app_path: &Path,
    build_dir: &Path,
    app_module: &str,
) -> Result<AppManifest, String> {
    let json = run_extraction_script(app_path, app_module).await?;
    let manifest = deserialize_manifest(&json)?;
    run_validation(&manifest)?;
    write_manifest(&manifest, build_dir)?;
    Ok(manifest)
}

/// Run the Python manifest extraction script via `uv run`.
async fn run_extraction_script(app_path: &Path, app_module: &str) -> Result<String, String> {
    let uv = Uv::new().await?;
    let script = extraction_script(app_module);
    uv.run_python_code(app_path, &script, &[])
        .await
        .map_err(|e| format!("manifest extraction failed: {e}"))?
        .into_stdout("manifest")
        .map_err(|e| format!("manifest extraction failed: {e}"))
}

/// Build the inline Python script that invokes `apx._manifest`.
fn extraction_script(app_module: &str) -> String {
    format!(
        r#"import sys; sys.argv = ['_manifest', '{app_module}']
import apx._manifest; apx._manifest.main()"#
    )
}

/// Deserialize JSON output into an `AppManifest`.
fn deserialize_manifest(json: &str) -> Result<AppManifest, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid manifest JSON: {e}"))
}

/// Run structural validation checks and report failures.
fn run_validation(manifest: &AppManifest) -> Result<(), String> {
    use std::fmt::Write;

    let checks = manifest::validate::validate_all(manifest);
    let failures: Vec<_> = checks.iter().filter(|c| !c.passed).collect();
    if failures.is_empty() {
        return Ok(());
    }
    let mut msg = String::from("manifest validation failed:\n");
    for f in &failures {
        let _ = writeln!(msg, "  - {}: {}", f.name, f.detail.as_deref().unwrap_or(""));
    }
    Err(msg)
}

/// Write the manifest to `<build_dir>/manifest.json`.
fn write_manifest(manifest: &AppManifest, build_dir: &Path) -> Result<(), String> {
    let path = build_dir.join(MANIFEST_FILENAME);
    manifest::save(manifest, &path).map_err(|e| format!("failed to write manifest: {e}"))
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use apx_framework::route::*;

    fn minimal_manifest_json() -> String {
        serde_json::to_string(&AppManifest {
            meta: None,
            routes: Vec::new(),
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
            has_middleware: false,
        })
        .unwrap()
    }

    #[test]
    fn deserialize_manifest_valid() {
        let json = minimal_manifest_json();
        let result = deserialize_manifest(&json);
        assert!(result.is_ok());
    }

    #[test]
    fn deserialize_manifest_invalid() {
        let result = deserialize_manifest("{not valid json}");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid manifest JSON"));
    }

    #[test]
    fn run_validation_pass() {
        let manifest = AppManifest {
            meta: None,
            routes: vec![RouteManifest {
                kind: HandlerKind::RequestResponse,
                method: HttpMethod::Get,
                path: RoutePath::new("/health").unwrap(),
                handler_qualname: QualName::new("mod.handler").unwrap(),
                params: Vec::new(),
                response_type: ResponseType::RawResponse,
                tags: Vec::new(),

                dependency_plan: None,
                status_code: 200,
                summary: None,
                description: None,
                include_in_schema: true,
                deprecated: false,
                operation_id: None,
                is_async_handler: true,
                dispatch_strategy: DispatchStrategy::default(),
            }],
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
            has_middleware: false,
        };
        assert!(run_validation(&manifest).is_ok());
    }

    #[test]
    fn run_validation_fail_duplicate_routes() {
        let route = RouteManifest {
            kind: HandlerKind::RequestResponse,
            method: HttpMethod::Get,
            path: RoutePath::new("/items").unwrap(),
            handler_qualname: QualName::new("mod.handler").unwrap(),
            params: Vec::new(),
            response_type: ResponseType::RawResponse,
            tags: Vec::new(),
            dependency_plan: None,
            status_code: 200,
            summary: None,
            description: None,
            include_in_schema: true,
            deprecated: false,
            operation_id: None,
            is_async_handler: true,
            dispatch_strategy: DispatchStrategy::default(),
        };
        let manifest = AppManifest {
            meta: None,
            routes: vec![route.clone(), route],
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
            has_middleware: false,
        };
        let result = run_validation(&manifest);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no_duplicate_routes"));
    }

    #[test]
    fn extraction_script_format() {
        let script = extraction_script("backend.app");
        assert!(script.contains("backend.app"));
        assert!(script.contains("apx._manifest"));
    }
}
