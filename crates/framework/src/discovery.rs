//! Python app discovery: import module, extract routes, bind to runtime objects.
//!
//! Two phases:
//! 1. **Analyze** (build-time): import module → extract [`AppManifest`] (serializable)
//! 2. **Bind** (runtime): resolve manifest entries to live Python objects → [`BoundRoute`]

use crate::pyapi::{ParamInfo, RouteInfo};
use crate::route::{
    AppManifest, AppModule, BodyLimit, BoundParam, BoundRoute, DispatchStrategy, HandlerKind,
    HttpMethod, ParamManifest, ParamSource, QualName, ResponseType, RouteManifest, RoutePath,
};
use pyo3::types::{PyAnyMethods, PyListMethods, PyString};
use pyo3::{Py, PyAny, Python};

/// Errors during app discovery.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// Python import or attribute access failed.
    #[error("python error: {0}")]
    Python(String),
    /// The module does not contain an `App` instance.
    #[error("no App instance found in module '{0}'")]
    NoApp(String),
    /// Route manifest extraction failed.
    #[error("invalid route: {0}")]
    InvalidRoute(String),
}

/// Discover routes and bind them in one step (dev mode).
///
/// Imports the Python module, finds the `App` instance, extracts route
/// metadata, and resolves Python types for runtime dispatch.
///
/// # Errors
///
/// Returns an error if the module cannot be imported, no `App` is found,
/// or route metadata is malformed.
pub fn discover_and_bind(
    py: Python<'_>,
    app_module: &AppModule,
) -> Result<(Vec<BoundRoute>, AppManifest), DiscoveryError> {
    let app = import_and_find_app(py, app_module)?;
    let manifest = extract_manifest(py, &app, app_module)?;
    let routes = bind_routes(py, &app, &manifest)?;
    Ok((routes, manifest))
}

/// Import the Python module and find the `App` instance.
fn import_and_find_app<'py>(
    py: Python<'py>,
    app_module: &AppModule,
) -> Result<pyo3::Bound<'py, PyAny>, DiscoveryError> {
    let module = py
        .import(PyString::new(py, app_module.as_str()))
        .map_err(|e| DiscoveryError::Python(format!("import '{app_module}': {e}")))?;

    // Walk module attributes looking for an App instance.
    let app_cls = py
        .import(c"apx._framework.app")
        .and_then(|m| m.getattr(c"App"))
        .map_err(|e| DiscoveryError::Python(format!("import apx._framework.app: {e}")))?;

    let dir = module
        .dir()
        .map_err(|e| DiscoveryError::Python(format!("dir({app_module}): {e}")))?;

    for name in &dir {
        let Ok(attr_name) = name.extract::<String>() else {
            continue;
        };
        // Skip dunder attributes.
        if attr_name.starts_with('_') {
            continue;
        }
        let Ok(attr) = module.getattr(PyString::new(py, &attr_name)) else {
            continue;
        };
        if attr.is_instance(&app_cls).unwrap_or(false) {
            return Ok(attr);
        }
    }

    // Also check for a conventional `app` attribute.
    if let Ok(attr) = module.getattr(c"app")
        && attr.is_instance(&app_cls).unwrap_or(false)
    {
        return Ok(attr);
    }

    Err(DiscoveryError::NoApp(app_module.as_str().to_owned()))
}

/// Extract an [`AppManifest`] from a Python `App` instance.
fn extract_manifest(
    py: Python<'_>,
    app: &pyo3::Bound<'_, PyAny>,
    _app_module: &AppModule,
) -> Result<AppManifest, DiscoveryError> {
    let max_body_limit = app
        .getattr(c"_max_body_limit")
        .and_then(|v| v.extract::<usize>())
        .unwrap_or(BodyLimit::DEFAULT.0);

    let routes_list = app
        .getattr(c"_routes")
        .map_err(|e| DiscoveryError::Python(format!("get _routes from App: {e}")))?;

    let routes_list = routes_list
        .cast::<pyo3::types::PyList>()
        .map_err(|e| DiscoveryError::Python(format!("_routes is not a list: {e}")))?;

    let mut routes = Vec::with_capacity(routes_list.len());
    for route_obj in routes_list {
        let manifest = extract_route_manifest(py, &route_obj)?;
        routes.push(manifest);
    }

    Ok(AppManifest {
        meta: None,
        routes,
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: BodyLimit(max_body_limit),
        validation_results: Vec::new(),
    })
}

/// Extract a single [`RouteManifest`] from a Rust-backed `RouteInfo` pyclass.
fn extract_route_manifest(
    _py: Python<'_>,
    route_obj: &pyo3::Bound<'_, PyAny>,
) -> Result<RouteManifest, DiscoveryError> {
    // Downcast to our Rust #[pyclass] type — direct field access.
    let route_info = route_obj
        .cast::<RouteInfo>()
        .map_err(|e| DiscoveryError::Python(format!("route is not a RouteInfo: {e}")))?;
    let ri = route_info.get();

    let method: HttpMethod = ri.method.into();
    let route_path = RoutePath::new(&ri.path)
        .map_err(|e| DiscoveryError::InvalidRoute(format!("path '{}': {e}", ri.path)))?;
    let qual = QualName::new(&ri.handler_qualname).map_err(|e| {
        DiscoveryError::InvalidRoute(format!("qualname '{}': {e}", ri.handler_qualname))
    })?;

    let params = ri
        .params
        .iter()
        .map(convert_param_info)
        .collect::<Result<Vec<_>, _>>()?;

    let response_type = parse_response_type(&ri.response_type)?;

    Ok(RouteManifest {
        kind: HandlerKind::RequestResponse,
        method,
        path: route_path,
        handler_qualname: qual,
        params,
        response_type,
        tags: ri.tags.clone(),
        dispatch_strategy: DispatchStrategy::Direct,
        dependency_plan: None,
        status_code: 200,
        summary: None,
        description: None,
        include_in_schema: true,
        deprecated: false,
        operation_id: None,
    })
}

/// Convert a `ParamInfo` pyclass to a [`ParamManifest`].
fn convert_param_info(param: &ParamInfo) -> Result<ParamManifest, DiscoveryError> {
    let type_qualname = QualName::new(&param.type_qualname).map_err(|e| {
        DiscoveryError::InvalidRoute(format!(
            "param '{}' type '{}': {e}",
            param.name, param.type_qualname
        ))
    })?;
    let source = parse_param_source(&param.source)?;

    Ok(ParamManifest {
        name: param.name.clone(),
        source,
        type_qualname,
        required: param.required,
        json_schema: None,
        alias: None,
        default_json: None,
    })
}

/// Parse a param source string from Python to [`ParamSource`].
fn parse_param_source(source: &str) -> Result<ParamSource, DiscoveryError> {
    match source {
        "path" => Ok(ParamSource::Path),
        "query" => Ok(ParamSource::Query),
        "header" => Ok(ParamSource::Header),
        "cookie" => Ok(ParamSource::Cookie),
        "body" => Ok(ParamSource::Body),
        "raw_body" => Ok(ParamSource::RawBody),
        "raw_request" => Ok(ParamSource::RawRequest),
        other => Err(DiscoveryError::InvalidRoute(format!(
            "unknown param source: {other}"
        ))),
    }
}

/// Parse a response type string from Python.
fn parse_response_type(s: &str) -> Result<ResponseType, DiscoveryError> {
    if let Some(qualname_str) = s.strip_prefix("model:") {
        let qualname = QualName::new(qualname_str).map_err(|e| {
            DiscoveryError::InvalidRoute(format!("response model '{qualname_str}': {e}"))
        })?;
        Ok(ResponseType::Model {
            qualname,
            json_schema: None,
            status_code: 200,
        })
    } else {
        Ok(ResponseType::RawResponse)
    }
}

// ── Binding (runtime) ───────────────────────────────────────────────────

/// Bind manifest routes to live Python objects for runtime dispatch.
fn bind_routes(
    py: Python<'_>,
    app: &pyo3::Bound<'_, PyAny>,
    manifest: &AppManifest,
) -> Result<Vec<BoundRoute>, DiscoveryError> {
    let routes_list = app
        .getattr(c"_routes")
        .map_err(|e| DiscoveryError::Python(format!("get _routes: {e}")))?;

    let routes_list = routes_list
        .cast::<pyo3::types::PyList>()
        .map_err(|e| DiscoveryError::Python(format!("_routes is not a list: {e}")))?;

    let mut bound = Vec::with_capacity(manifest.routes.len());

    for (i, route_manifest) in manifest.routes.iter().enumerate() {
        let route_obj = routes_list
            .get_item(i)
            .map_err(|e| DiscoveryError::Python(format!("get route {i}: {e}")))?;

        // Downcast to Rust RouteInfo — direct access to handler field.
        let route_info = route_obj
            .cast::<RouteInfo>()
            .map_err(|e| DiscoveryError::Python(format!("route {i} is not RouteInfo: {e}")))?;
        let handler = route_info.get().handler.clone_ref(py);

        let params = bind_params(py, route_manifest)?;
        let response_model = resolve_response_model(py, route_manifest)?;

        let has_body_param = route_manifest.params.iter().any(|p| {
            matches!(
                p.source,
                ParamSource::Body | ParamSource::RawBody | ParamSource::RawRequest
            )
        });

        bound.push(BoundRoute {
            manifest: route_manifest.clone(),
            handler,
            params,
            response_model,
            has_body_param,
        });
    }

    Ok(bound)
}

/// Bind parameter manifests to their Python types.
fn bind_params(py: Python<'_>, route: &RouteManifest) -> Result<Vec<BoundParam>, DiscoveryError> {
    let mut bound = Vec::with_capacity(route.params.len());

    for param in &route.params {
        let python_type = resolve_param_type(py, param)?;
        bound.push(BoundParam {
            manifest: param.clone(),
            python_type,
        });
    }

    Ok(bound)
}

/// Resolve the Python type for a parameter (for Body params that need Pydantic validation).
fn resolve_param_type(
    py: Python<'_>,
    param: &ParamManifest,
) -> Result<Option<Py<PyAny>>, DiscoveryError> {
    match param.source {
        ParamSource::Body => {
            let cls = import_qualified_name(py, param.type_qualname.as_str())?;
            Ok(Some(cls))
        }
        _ => Ok(None),
    }
}

/// Resolve the response model class for type checking.
fn resolve_response_model(
    py: Python<'_>,
    route: &RouteManifest,
) -> Result<Option<Py<PyAny>>, DiscoveryError> {
    match &route.response_type {
        ResponseType::Model { qualname, .. } => {
            let cls = import_qualified_name(py, qualname.as_str())?;
            Ok(Some(cls))
        }
        ResponseType::StreamingResponse | ResponseType::RawResponse => Ok(None),
    }
}

/// Import a Python class by its qualified name (e.g. `"backend.app.ItemCreate"`).
fn import_qualified_name(py: Python<'_>, qualname: &str) -> Result<Py<PyAny>, DiscoveryError> {
    // Split into module path and class name.
    let (module_path, class_name) = qualname.rsplit_once('.').ok_or_else(|| {
        DiscoveryError::InvalidRoute(format!(
            "qualified name '{qualname}' has no module component"
        ))
    })?;

    let module = py
        .import(PyString::new(py, module_path))
        .map_err(|e| DiscoveryError::Python(format!("import '{module_path}': {e}")))?;

    let cls = module.getattr(PyString::new(py, class_name)).map_err(|e| {
        DiscoveryError::Python(format!("get '{class_name}' from '{module_path}': {e}"))
    })?;

    Ok(cls.unbind())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use crate::pyapi::ParamInfo;

    // ── parse_param_source ───────────────────────────────────────────────

    #[test]
    fn parse_param_source_path() {
        assert_eq!(parse_param_source("path").unwrap(), ParamSource::Path);
    }

    #[test]
    fn parse_param_source_query() {
        assert_eq!(parse_param_source("query").unwrap(), ParamSource::Query);
    }

    #[test]
    fn parse_param_source_header() {
        assert_eq!(parse_param_source("header").unwrap(), ParamSource::Header);
    }

    #[test]
    fn parse_param_source_cookie() {
        assert_eq!(parse_param_source("cookie").unwrap(), ParamSource::Cookie);
    }

    #[test]
    fn parse_param_source_body() {
        assert_eq!(parse_param_source("body").unwrap(), ParamSource::Body);
    }

    #[test]
    fn parse_param_source_raw_body() {
        assert_eq!(
            parse_param_source("raw_body").unwrap(),
            ParamSource::RawBody
        );
    }

    #[test]
    fn parse_param_source_raw_request() {
        assert_eq!(
            parse_param_source("raw_request").unwrap(),
            ParamSource::RawRequest
        );
    }

    #[test]
    fn parse_param_source_unknown() {
        let err = parse_param_source("unknown").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
        let msg = format!("{err}");
        assert!(msg.contains("unknown"));
    }

    // ── parse_response_type ──────────────────────────────────────────────

    #[test]
    fn parse_response_type_model_valid() {
        let rt = parse_response_type("model:backend.models.Item").unwrap();
        match rt {
            ResponseType::Model {
                qualname,
                json_schema,
                status_code,
            } => {
                assert_eq!(qualname.as_str(), "backend.models.Item");
                assert!(json_schema.is_none());
                assert_eq!(status_code, 200);
            }
            _ => panic!("expected Model"),
        }
    }

    #[test]
    fn parse_response_type_model_invalid_qualname() {
        let err = parse_response_type("model:").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
    }

    #[test]
    fn parse_response_type_raw_fallback() {
        let rt = parse_response_type("raw_response").unwrap();
        assert!(matches!(rt, ResponseType::RawResponse));
    }

    #[test]
    fn parse_response_type_anything_else_is_raw() {
        let rt = parse_response_type("something_else").unwrap();
        assert!(matches!(rt, ResponseType::RawResponse));
    }

    // ── convert_param_info ───────────────────────────────────────────────

    #[test]
    fn convert_param_info_valid() {
        let param = ParamInfo {
            name: "item_id".to_owned(),
            type_qualname: "int".to_owned(),
            source: "path".to_owned(),
            required: true,
        };
        let manifest = convert_param_info(&param).unwrap();
        assert_eq!(manifest.name, "item_id");
        assert_eq!(manifest.type_qualname.as_str(), "int");
        assert_eq!(manifest.source, ParamSource::Path);
        assert!(manifest.required);
    }

    #[test]
    fn convert_param_info_invalid_qualname() {
        let param = ParamInfo {
            name: "bad".to_owned(),
            type_qualname: String::new(),
            source: "path".to_owned(),
            required: true,
        };
        let err = convert_param_info(&param).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
    }

    #[test]
    fn convert_param_info_invalid_source() {
        let param = ParamInfo {
            name: "x".to_owned(),
            type_qualname: "int".to_owned(),
            source: "nonsense".to_owned(),
            required: false,
        };
        let err = convert_param_info(&param).unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
    }

    // ── DiscoveryError Display ───────────────────────────────────────────

    #[test]
    fn discovery_error_display_python() {
        let err = DiscoveryError::Python("import failed".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("import failed"));
    }

    #[test]
    fn discovery_error_display_no_app() {
        let err = DiscoveryError::NoApp("backend.app".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("backend.app"));
    }

    #[test]
    fn discovery_error_display_invalid_route() {
        let err = DiscoveryError::InvalidRoute("bad path".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("bad path"));
    }
}
