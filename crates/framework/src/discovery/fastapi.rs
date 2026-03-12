//! FastAPI-specific discovery: find app, walk routes, read `Dependant`.

use crate::discovery::DiscoveryError;
use crate::route::AppModule;
use pyo3::types::{PyAnyMethods, PyString};
use pyo3::{Bound, Python};

/// Find a `FastAPI` instance in the given module.
///
/// Checks the conventional `app` attribute first, then walks `dir(module)`.
///
/// Used by both production serving (manifest-based binding) and live discovery (tests/dev).
pub fn find_fastapi_app<'py>(
    py: Python<'py>,
    app_module: &AppModule,
) -> Result<Bound<'py, pyo3::PyAny>, DiscoveryError> {
    let module = py
        .import(PyString::new(py, app_module.as_str()))
        .map_err(|e| DiscoveryError::Python(format!("import '{app_module}': {e}")))?;

    let fastapi_cls = py
        .import(c"fastapi")
        .and_then(|m| m.getattr(c"FastAPI"))
        .map_err(|e| DiscoveryError::Python(format!("import fastapi.FastAPI: {e}")))?;

    // Check conventional `app` attribute first.
    if let Ok(attr) = module.getattr(c"app")
        && attr.is_instance(&fastapi_cls).unwrap_or(false)
    {
        return Ok(attr);
    }

    // Walk all non-dunder attributes.
    let dir = module
        .dir()
        .map_err(|e| DiscoveryError::Python(format!("dir({app_module}): {e}")))?;

    for name in &dir {
        let Ok(attr_name) = name.extract::<String>() else {
            continue;
        };
        if attr_name.starts_with('_') {
            continue;
        }
        let Ok(attr) = module.getattr(PyString::new(py, &attr_name)) else {
            continue;
        };
        if attr.is_instance(&fastapi_cls).unwrap_or(false) {
            return Ok(attr);
        }
    }

    Err(DiscoveryError::NoApp(app_module.as_str().to_owned()))
}

// ── Live import via Python manifest ──────────────────────────────────────

/// Import the app and extract a full manifest by calling Python's
/// `apx._manifest.compile_manifest()` in the embedded interpreter.
///
/// This is the live-import path: the app is imported in the current process
/// and routes are introspected with full dispatch classification and
/// dependency plan compilation.
pub fn live_extract_manifest(
    py: Python<'_>,
    app_module: &AppModule,
) -> Result<crate::route::AppManifest, DiscoveryError> {
    // Ensure cwd and src/ are on sys.path (mirrors what _manifest.py does).
    let sys = py
        .import(c"sys")
        .map_err(|e| DiscoveryError::Python(format!("import sys: {e}")))?;
    let sys_path = sys
        .getattr(c"path")
        .map_err(|e| DiscoveryError::Python(format!("sys.path: {e}")))?;
    let os = py
        .import(c"os")
        .map_err(|e| DiscoveryError::Python(format!("import os: {e}")))?;
    let cwd: String = os
        .call_method0(c"getcwd")
        .and_then(|v| v.extract())
        .map_err(|e| DiscoveryError::Python(format!("os.getcwd: {e}")))?;
    let _ = sys_path.call_method1(c"insert", (0i32, &cwd));
    let src = format!("{cwd}/src");
    if std::path::Path::new(&src).is_dir() {
        let _ = sys_path.call_method1(c"insert", (0i32, &src));
    }

    // Call apx._manifest.compile_manifest(app_module)
    let manifest_mod = py
        .import(c"apx._manifest")
        .map_err(|e| DiscoveryError::Python(format!("import apx._manifest: {e}")))?;
    let result = manifest_mod
        .call_method1(c"compile_manifest", (app_module.as_str(),))
        .map_err(|e| DiscoveryError::Python(format!("compile_manifest: {e}")))?;

    // Serialize to JSON string, then deserialize into Rust struct.
    let json_mod = py
        .import(c"json")
        .map_err(|e| DiscoveryError::Python(format!("import json: {e}")))?;
    let json_str: String = json_mod
        .call_method1(c"dumps", (&result,))
        .and_then(|v| v.extract())
        .map_err(|e| DiscoveryError::Python(format!("json.dumps: {e}")))?;

    serde_json::from_str(&json_str)
        .map_err(|e| DiscoveryError::Python(format!("deserialize manifest from Python: {e}")))
}

// ── Live discovery (Rust-native extraction) ─────────────────────────────
//
// The functions below extract route metadata by walking the live FastAPI
// `app.routes` list. Production serving uses `bind_routes_from_manifest`
// instead, which re-uses the already-extracted manifest.
//
// These functions are un-gated (not `#[cfg(test)]`) so they are available
// for future `apx dev` live-reload support. They are currently exercised
// only by integration tests.

#[allow(dead_code)]
pub fn import_and_extract<'py>(
    py: Python<'py>,
    app_module: &AppModule,
) -> Result<(Bound<'py, pyo3::PyAny>, crate::route::AppManifest), DiscoveryError> {
    let app = find_fastapi_app(py, app_module)?;
    let routes = extract_routes(py, &app)?;

    let has_middleware: bool = app
        .getattr(c"user_middleware")
        .and_then(|mw| mw.len())
        .map(|n| n > 0)
        .unwrap_or(false);

    let manifest = crate::route::AppManifest {
        meta: None,
        routes,
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: crate::route::BodyLimit::DEFAULT,
        validation_results: Vec::new(),
        has_middleware,
    };

    Ok((app, manifest))
}

#[allow(dead_code)]
/// Metadata extracted from a single `APIRoute` Python object.
struct RouteMetadata {
    path: String,
    methods: std::collections::HashSet<String>,
    status_code: u16,
    tags: Vec<String>,
    summary: Option<String>,
    description: Option<String>,
    deprecated: bool,
    include_in_schema: bool,
    operation_id: Option<String>,
}

#[allow(dead_code)]
/// Raw attributes extracted from a Python `APIRoute` via `FromPyObject`.
#[derive(pyo3::FromPyObject)]
struct RouteAttrs {
    #[pyo3(attribute)]
    path: String,
    #[pyo3(attribute, default)]
    methods: Option<std::collections::HashSet<String>>,
    #[pyo3(attribute, default)]
    status_code: Option<u16>,
    #[pyo3(attribute, default)]
    tags: Option<Vec<String>>,
    #[pyo3(attribute, default)]
    summary: Option<String>,
    #[pyo3(attribute, default)]
    description: Option<String>,
    #[pyo3(attribute, default)]
    deprecated: Option<bool>,
    #[pyo3(attribute, default)]
    include_in_schema: Option<bool>,
    #[pyo3(attribute, default)]
    operation_id: Option<String>,
}

#[allow(dead_code)]
/// Read scalar metadata fields from a Python `APIRoute`.
fn extract_route_metadata(route: &Bound<'_, pyo3::PyAny>) -> Result<RouteMetadata, DiscoveryError> {
    let raw: RouteAttrs = route
        .extract()
        .map_err(|e| DiscoveryError::Python(format!("route metadata: {e}")))?;
    Ok(RouteMetadata {
        path: raw.path,
        methods: raw.methods.unwrap_or_default(),
        status_code: raw.status_code.unwrap_or(200),
        tags: raw.tags.unwrap_or_default(),
        summary: raw.summary,
        description: raw.description,
        deprecated: raw.deprecated.unwrap_or(false),
        include_in_schema: raw.include_in_schema.unwrap_or(true),
        operation_id: raw.operation_id,
    })
}

#[allow(dead_code)]
/// Determine handler kind and response type for a route.
fn classify_route(
    py: Python<'_>,
    endpoint: &Bound<'_, pyo3::PyAny>,
    _dependant: &Bound<'_, pyo3::PyAny>,
    response_model: &Bound<'_, pyo3::PyAny>,
) -> Result<(crate::route::HandlerKind, crate::route::ResponseType), DiscoveryError> {
    let response_type = classify_response_type(py, response_model)?;
    let kind = classify_handler_kind(py, endpoint)?;
    Ok((kind, response_type))
}

#[allow(dead_code)]
/// Walk `app.routes`, filter `APIRoute`, extract [`RouteManifest`] for each.
fn extract_routes(
    py: Python<'_>,
    app: &Bound<'_, pyo3::PyAny>,
) -> Result<Vec<crate::route::RouteManifest>, DiscoveryError> {
    use crate::route::RoutePath;

    let routes_obj = app
        .getattr(c"routes")
        .map_err(|e| DiscoveryError::Python(format!("get routes from app: {e}")))?;

    let routes_list = routes_obj
        .cast::<pyo3::types::PyList>()
        .map_err(|e| DiscoveryError::Python(format!("app.routes is not a list: {e}")))?;

    let api_route_cls = py
        .import(c"fastapi.routing")
        .and_then(|m| m.getattr(c"APIRoute"))
        .map_err(|e| DiscoveryError::Python(format!("import fastapi.routing.APIRoute: {e}")))?;

    let ws_route_cls = py
        .import(c"fastapi.routing")
        .and_then(|m| m.getattr(c"APIWebSocketRoute"))
        .ok();

    let mut manifests = Vec::new();

    for route in routes_list {
        // WebSocket routes
        if let Some(ref ws_cls) = ws_route_cls
            && route.is_instance(ws_cls).unwrap_or(false)
        {
            if let Some(manifest) = extract_ws_route(py, &route)? {
                manifests.push(manifest);
            }
            continue;
        }

        if !route.is_instance(&api_route_cls).unwrap_or(false) {
            continue;
        }

        let meta = extract_route_metadata(&route)?;
        let handler_qualname = get_handler_qualname(
            &route
                .getattr(c"endpoint")
                .map_err(|e| DiscoveryError::Python(format!("route.endpoint: {e}")))?,
        )?;

        let endpoint = route
            .getattr(c"endpoint")
            .map_err(|e| DiscoveryError::Python(format!("route.endpoint: {e}")))?;
        let dependant = route
            .getattr(c"dependant")
            .map_err(|e| DiscoveryError::Python(format!("route.dependant: {e}")))?;
        let response_model = route
            .getattr(c"response_model")
            .map_err(|e| DiscoveryError::Python(format!("route.response_model: {e}")))?;

        let params = extract_params_from_dependant(py, &dependant)?;
        let (kind, response_type) = classify_route(py, &endpoint, &dependant, &response_model)?;

        let is_async_handler = {
            let inspect = py
                .import(c"inspect")
                .map_err(|e| DiscoveryError::Python(format!("import inspect: {e}")))?;
            inspect
                .call_method1(c"iscoroutinefunction", (&endpoint,))
                .is_ok_and(|r| r.is_truthy().unwrap_or(false))
        };

        let route_path = RoutePath::new(&meta.path)
            .map_err(|e| DiscoveryError::InvalidRoute(format!("path '{}': {e}", meta.path)))?;

        // One RouteManifest per HTTP method (FastAPI stores methods as a set).
        for method_str in &meta.methods {
            let method = super::parse_http_method(method_str)?;
            manifests.push(crate::route::RouteManifest {
                kind,
                method,
                path: route_path.clone(),
                handler_qualname: handler_qualname.clone(),
                params: params.clone(),
                response_type: response_type.clone(),
                tags: meta.tags.clone(),
                dependency_plan: None,
                status_code: meta.status_code,
                summary: meta.summary.clone(),
                description: meta.description.clone(),
                include_in_schema: meta.include_in_schema,
                deprecated: meta.deprecated,
                operation_id: meta.operation_id.clone(),
                is_async_handler,
                dispatch_strategy: crate::route::DispatchStrategy::default(),
            });
        }
    }

    Ok(manifests)
}

#[allow(dead_code)]
/// Extract a WebSocket route from an `APIWebSocketRoute`.
fn extract_ws_route(
    py: Python<'_>,
    route: &Bound<'_, pyo3::PyAny>,
) -> Result<Option<crate::route::RouteManifest>, DiscoveryError> {
    use crate::route::{HandlerKind, ResponseType, RoutePath};

    let path: String = extract_attr(route, "path")?;

    let Ok(endpoint) = route.getattr(c"endpoint") else {
        return Ok(None);
    };

    let handler_qualname = get_handler_qualname(&endpoint)?;

    let route_path = RoutePath::new(&path)
        .map_err(|e| DiscoveryError::InvalidRoute(format!("ws path '{path}': {e}")))?;

    let name: Option<String> = route
        .getattr(c"name")
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .ok();

    let _ = py; // suppress unused warning

    Ok(Some(crate::route::RouteManifest {
        kind: HandlerKind::WebSocket,
        method: crate::route::HttpMethod::Get,
        path: route_path,
        handler_qualname,
        params: Vec::new(),
        response_type: ResponseType::RawResponse,
        tags: Vec::new(),
        dependency_plan: None,
        status_code: 101,
        summary: name,
        description: None,
        include_in_schema: false,
        deprecated: false,
        operation_id: None,
        is_async_handler: true,
        dispatch_strategy: crate::route::DispatchStrategy::default(),
    }))
}

#[allow(dead_code)]
/// Extract a string attribute from a Python object.
fn extract_attr(obj: &Bound<'_, pyo3::PyAny>, attr: &str) -> Result<String, DiscoveryError> {
    let val = obj
        .getattr(PyString::new(obj.py(), attr))
        .map_err(|e| DiscoveryError::Python(format!("route.{attr}: {e}")))?;
    val.extract::<String>()
        .map_err(|e| DiscoveryError::Python(format!("route.{attr} extract: {e}")))
}

#[allow(dead_code)]
/// Read `dependant.{path,query,header,cookie,body}_params` → `Vec<ParamManifest>`.
fn extract_params_from_dependant(
    py: Python<'_>,
    dependant: &Bound<'_, pyo3::PyAny>,
) -> Result<Vec<crate::route::ParamManifest>, DiscoveryError> {
    use crate::route::ParamSource;

    let mut params = Vec::new();

    let param_groups: &[(&str, ParamSource)] = &[
        ("path_params", ParamSource::Path),
        ("query_params", ParamSource::Query),
        ("header_params", ParamSource::Header),
        ("cookie_params", ParamSource::Cookie),
        ("body_params", ParamSource::Body),
    ];

    for &(attr_name, source) in param_groups {
        let group = dependant
            .getattr(PyString::new(py, attr_name))
            .map_err(|e| DiscoveryError::Python(format!("dependant.{attr_name}: {e}")))?;

        let group_list = group.cast::<pyo3::types::PyList>().map_err(|e| {
            DiscoveryError::Python(format!("dependant.{attr_name} is not a list: {e}"))
        })?;

        for field in group_list {
            params.push(field_to_param_manifest(py, &field, source)?);
        }
    }

    Ok(params)
}

#[allow(dead_code)]
/// Raw attributes extracted from a Python field info object.
#[derive(pyo3::FromPyObject)]
struct FieldAttrs<'py> {
    #[pyo3(attribute)]
    name: String,
    #[pyo3(attribute, default)]
    alias: Option<String>,
    #[pyo3(attribute, default)]
    required: Option<bool>,
    #[pyo3(attribute("type_"))]
    type_obj: Bound<'py, pyo3::PyAny>,
}

#[allow(dead_code)]
/// Convert a FastAPI `FieldInfo` / `ModelField` → [`ParamManifest`].
fn field_to_param_manifest(
    py: Python<'_>,
    field: &Bound<'_, pyo3::PyAny>,
    source: crate::route::ParamSource,
) -> Result<crate::route::ParamManifest, DiscoveryError> {
    use crate::route::QualName;

    let attrs: FieldAttrs<'_> = field
        .extract()
        .map_err(|e| DiscoveryError::Python(format!("field attrs: {e}")))?;

    let type_qualname_str = python_type_qualname(py, &attrs.type_obj)?;
    let type_qualname = QualName::new(&type_qualname_str).map_err(|e| {
        DiscoveryError::InvalidRoute(format!(
            "param '{}' type '{type_qualname_str}': {e}",
            attrs.name
        ))
    })?;

    let default_json = extract_default_json(py, field)?;

    // Only store alias if it differs from the name.
    let alias = attrs.alias.filter(|a| a != &attrs.name);

    Ok(crate::route::ParamManifest {
        name: attrs.name,
        source,
        type_qualname,
        required: attrs.required.unwrap_or(true),
        json_schema: None,
        alias,
        default_json,
    })
}

#[allow(dead_code)]
/// Extract `__module__.__qualname__` from a Python object.
fn extract_module_qualname(obj: &Bound<'_, pyo3::PyAny>) -> Option<String> {
    let module: String = obj
        .getattr(c"__module__")
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .ok()?;
    let qualname: String = obj
        .getattr(c"__qualname__")
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .ok()?;
    if module == "builtins" || module.is_empty() {
        Some(qualname)
    } else {
        Some(format!("{module}.{qualname}"))
    }
}

#[allow(dead_code)]
/// Get the qualified name of a Python type.
fn python_type_qualname(
    _py: Python<'_>,
    type_obj: &Bound<'_, pyo3::PyAny>,
) -> Result<String, DiscoveryError> {
    if let Some(name) = extract_module_qualname(type_obj) {
        return Ok(name);
    }
    type_obj
        .str()
        .and_then(|v| v.extract::<String>())
        .map_err(|e| DiscoveryError::Python(format!("type qualname: {e}")))
}

#[allow(dead_code)]
/// Extract default value as JSON if the field has a default.
fn extract_default_json(
    py: Python<'_>,
    field: &Bound<'_, pyo3::PyAny>,
) -> Result<Option<serde_json::Value>, DiscoveryError> {
    let Ok(default_obj) = field.getattr(c"default") else {
        return Ok(None);
    };

    if default_obj.is_none() {
        return Ok(None);
    }

    if let Ok(repr) = default_obj.repr().and_then(|r| r.extract::<String>())
        && (repr.contains("PydanticUndefined") || repr.contains("MISSING"))
    {
        return Ok(None);
    }

    let json_mod = py
        .import(c"json")
        .map_err(|e| DiscoveryError::Python(format!("import json: {e}")))?;
    let json_str: String = json_mod
        .call_method1(c"dumps", (&default_obj,))
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .map_err(|e| DiscoveryError::Python(format!("json.dumps(default): {e}")))?;
    let value: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| DiscoveryError::Python(format!("parse default JSON: {e}")))?;
    Ok(Some(value))
}

#[allow(dead_code)]
/// Get the handler's qualified name.
fn get_handler_qualname(
    endpoint: &Bound<'_, pyo3::PyAny>,
) -> Result<crate::route::QualName, DiscoveryError> {
    let full = extract_module_qualname(endpoint).ok_or_else(|| {
        DiscoveryError::Python("endpoint missing __module__ or __qualname__".to_owned())
    })?;
    crate::route::QualName::new(&full)
        .map_err(|e| DiscoveryError::InvalidRoute(format!("handler qualname '{full}': {e}")))
}

#[allow(dead_code)]
/// Classify the response type from `route.response_model`.
fn classify_response_type(
    py: Python<'_>,
    response_model: &Bound<'_, pyo3::PyAny>,
) -> Result<crate::route::ResponseType, DiscoveryError> {
    use crate::route::{QualName, ResponseType};

    if response_model.is_none() {
        return Ok(ResponseType::RawResponse);
    }

    if let Ok(streaming_cls) = py
        .import(c"starlette.responses")
        .and_then(|m| m.getattr(c"StreamingResponse"))
        && (response_model.is_instance(&streaming_cls).unwrap_or(false)
            || response_model.eq(&streaming_cls).unwrap_or(false))
    {
        return Ok(ResponseType::StreamingResponse);
    }

    let qualname_str = python_type_qualname(py, response_model)?;
    let qualname = QualName::new(&qualname_str).map_err(|e| {
        DiscoveryError::InvalidRoute(format!("response model '{qualname_str}': {e}"))
    })?;

    Ok(ResponseType::Model {
        qualname,
        json_schema: None,
        status_code: 200,
    })
}

#[allow(dead_code)]
/// Classify the handler kind (request-response, SSE, websocket).
fn classify_handler_kind(
    py: Python<'_>,
    endpoint: &Bound<'_, pyo3::PyAny>,
) -> Result<crate::route::HandlerKind, DiscoveryError> {
    use crate::route::HandlerKind;

    if let Ok(ann) = endpoint.getattr(c"__annotations__")
        && let Ok(ret) = ann.get_item(c"return")
        && let Ok(streaming_cls) = py
            .import(c"starlette.responses")
            .and_then(|m| m.getattr(c"StreamingResponse"))
        && ret.eq(&streaming_cls).unwrap_or(false)
    {
        return Ok(HandlerKind::SSE);
    }

    let inspect = py
        .import(c"inspect")
        .map_err(|e| DiscoveryError::Python(format!("import inspect: {e}")))?;
    if let Ok(is_gen) = inspect.call_method1(c"isasyncgenfunction", (endpoint,))
        && is_gen.is_truthy().unwrap_or(false)
    {
        return Ok(HandlerKind::SSE);
    }

    Ok(HandlerKind::RequestResponse)
}
