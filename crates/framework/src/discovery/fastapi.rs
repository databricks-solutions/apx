//! FastAPI-specific discovery: find app, walk routes, read `Dependant`.

use crate::discovery::DiscoveryError;
use crate::route::{
    AppManifest, AppModule, BodyLimit, HandlerKind, ParamManifest, ParamSource, QualName,
    ResponseType, RouteManifest, RoutePath,
};
use pyo3::types::{PyAnyMethods, PyString};
use pyo3::{Bound, FromPyObject, Python};
use std::collections::HashSet;

/// Import the user module, find the FastAPI app, and extract an [`AppManifest`].
pub fn import_and_extract<'py>(
    py: Python<'py>,
    app_module: &AppModule,
) -> Result<(Bound<'py, pyo3::PyAny>, AppManifest), DiscoveryError> {
    let app = find_fastapi_app(py, app_module)?;
    let routes = extract_routes(py, &app)?;

    let manifest = AppManifest {
        meta: None,
        routes,
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: BodyLimit::DEFAULT,
        validation_results: Vec::new(),
    };

    Ok((app, manifest))
}

/// Find a `FastAPI` instance in the given module.
///
/// Checks the conventional `app` attribute first, then walks `dir(module)`.
fn find_fastapi_app<'py>(
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

/// Metadata extracted from a single `APIRoute` Python object.
struct RouteMetadata {
    path: String,
    methods: HashSet<String>,
    status_code: u16,
    tags: Vec<String>,
    summary: Option<String>,
    description: Option<String>,
    deprecated: bool,
    include_in_schema: bool,
    operation_id: Option<String>,
}

/// Raw attributes extracted from a Python `APIRoute` via `FromPyObject`.
#[derive(FromPyObject)]
struct RouteAttrs {
    #[pyo3(attribute)]
    path: String,
    #[pyo3(attribute, default)]
    methods: Option<HashSet<String>>,
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

/// Determine handler kind and response type for a route.
fn classify_route(
    py: Python<'_>,
    endpoint: &Bound<'_, pyo3::PyAny>,
    _dependant: &Bound<'_, pyo3::PyAny>,
    response_model: &Bound<'_, pyo3::PyAny>,
) -> Result<(HandlerKind, ResponseType), DiscoveryError> {
    let response_type = classify_response_type(py, response_model)?;
    let kind = classify_handler_kind(py, endpoint)?;
    Ok((kind, response_type))
}

/// Walk `app.routes`, filter `APIRoute`, extract [`RouteManifest`] for each.
fn extract_routes(
    py: Python<'_>,
    app: &Bound<'_, pyo3::PyAny>,
) -> Result<Vec<RouteManifest>, DiscoveryError> {
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
            manifests.push(RouteManifest {
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
            });
        }
    }

    Ok(manifests)
}

/// Extract a WebSocket route from an `APIWebSocketRoute`.
fn extract_ws_route(
    py: Python<'_>,
    route: &Bound<'_, pyo3::PyAny>,
) -> Result<Option<RouteManifest>, DiscoveryError> {
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

    Ok(Some(RouteManifest {
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
    }))
}

/// Extract a string attribute from a Python object.
fn extract_attr(obj: &Bound<'_, pyo3::PyAny>, attr: &str) -> Result<String, DiscoveryError> {
    let val = obj
        .getattr(PyString::new(obj.py(), attr))
        .map_err(|e| DiscoveryError::Python(format!("route.{attr}: {e}")))?;
    val.extract::<String>()
        .map_err(|e| DiscoveryError::Python(format!("route.{attr} extract: {e}")))
}

/// Read `dependant.{path,query,header,cookie,body}_params` → `Vec<ParamManifest>`.
fn extract_params_from_dependant(
    py: Python<'_>,
    dependant: &Bound<'_, pyo3::PyAny>,
) -> Result<Vec<ParamManifest>, DiscoveryError> {
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

/// Raw attributes extracted from a Python field info object.
#[derive(FromPyObject)]
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

/// Convert a FastAPI `FieldInfo` / `ModelField` → [`ParamManifest`].
fn field_to_param_manifest(
    py: Python<'_>,
    field: &Bound<'_, pyo3::PyAny>,
    source: ParamSource,
) -> Result<ParamManifest, DiscoveryError> {
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

    Ok(ParamManifest {
        name: attrs.name,
        source,
        type_qualname,
        required: attrs.required.unwrap_or(true),
        json_schema: None,
        alias,
        default_json,
    })
}

/// Get the qualified name of a Python type.
fn python_type_qualname(
    _py: Python<'_>,
    type_obj: &Bound<'_, pyo3::PyAny>,
) -> Result<String, DiscoveryError> {
    // Try __module__.__qualname__ for classes.
    if let (Ok(module), Ok(qualname)) = (
        type_obj
            .getattr(c"__module__")
            .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract::<String>()),
        type_obj
            .getattr(c"__qualname__")
            .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract::<String>()),
    ) {
        if module == "builtins" || module.is_empty() {
            return Ok(qualname);
        }
        return Ok(format!("{module}.{qualname}"));
    }

    // Fallback: str(type_).
    let s = type_obj
        .str()
        .and_then(|v| v.extract::<String>())
        .map_err(|e| DiscoveryError::Python(format!("type qualname: {e}")))?;
    Ok(s)
}

/// Extract default value as JSON if the field has a default.
fn extract_default_json(
    py: Python<'_>,
    field: &Bound<'_, pyo3::PyAny>,
) -> Result<Option<serde_json::Value>, DiscoveryError> {
    let Ok(default_obj) = field.getattr(c"default") else {
        return Ok(None);
    };

    // None in Python means no default (or explicit None default).
    if default_obj.is_none() {
        return Ok(None);
    }

    // Check for PydanticUndefined / dataclasses.MISSING sentinel.
    if let Ok(repr) = default_obj.repr().and_then(|r| r.extract::<String>())
        && (repr.contains("PydanticUndefined") || repr.contains("MISSING"))
    {
        return Ok(None);
    }

    // Try JSON serialization via Python's json.dumps.
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

/// Get the handler's qualified name.
fn get_handler_qualname(endpoint: &Bound<'_, pyo3::PyAny>) -> Result<QualName, DiscoveryError> {
    let module: String = endpoint
        .getattr(c"__module__")
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .map_err(|e| DiscoveryError::Python(format!("endpoint.__module__: {e}")))?;

    let qualname: String = endpoint
        .getattr(c"__qualname__")
        .and_then(|v: Bound<'_, pyo3::PyAny>| v.extract())
        .map_err(|e| DiscoveryError::Python(format!("endpoint.__qualname__: {e}")))?;

    let full = if module == "builtins" || module.is_empty() {
        qualname
    } else {
        format!("{module}.{qualname}")
    };

    QualName::new(&full)
        .map_err(|e| DiscoveryError::InvalidRoute(format!("handler qualname '{full}': {e}")))
}

/// Classify the response type from `route.response_model`.
fn classify_response_type(
    py: Python<'_>,
    response_model: &Bound<'_, pyo3::PyAny>,
) -> Result<ResponseType, DiscoveryError> {
    // If response_model is None, it's a raw response.
    if response_model.is_none() {
        return Ok(ResponseType::RawResponse);
    }

    // Check if it's a streaming response type.
    if let Ok(streaming_cls) = py
        .import(c"starlette.responses")
        .and_then(|m| m.getattr(c"StreamingResponse"))
        && (response_model.is_instance(&streaming_cls).unwrap_or(false)
            || response_model.eq(&streaming_cls).unwrap_or(false))
    {
        return Ok(ResponseType::StreamingResponse);
    }

    // It's a model — extract qualified name.
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

/// Classify the handler kind (request-response, SSE, websocket).
fn classify_handler_kind(
    py: Python<'_>,
    endpoint: &Bound<'_, pyo3::PyAny>,
) -> Result<HandlerKind, DiscoveryError> {
    // Check return type annotation for StreamingResponse.
    if let Ok(ann) = endpoint.getattr(c"__annotations__")
        && let Ok(ret) = ann.get_item(c"return")
        && let Ok(streaming_cls) = py
            .import(c"starlette.responses")
            .and_then(|m| m.getattr(c"StreamingResponse"))
        && ret.eq(&streaming_cls).unwrap_or(false)
    {
        return Ok(HandlerKind::SSE);
    }

    // Check if it's an async generator (SSE pattern).
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
