//! Manifest → `BoundRoute` binding: import qualnames, resolve types.

use crate::discovery::DiscoveryError;
use crate::route::{
    App, AppManifest, BoundRoute, DirectContext, DispatchStrategy, Handler, HandlerKind,
    ParamSource, ResponseType, RouteManifest,
};
use pyo3::types::{PyAnyMethods, PyString};
use pyo3::{Bound, Py, PyAny, Python};
use std::collections::{HashMap, HashSet};

/// A resolved HTTP endpoint: the handler callable and optional response model.
struct ResolvedEndpoint {
    handler: Py<PyAny>,
    response_model: Option<Py<PyAny>>,
}

/// Bind manifest routes to live Python handler objects for runtime dispatch.
///
/// Builds a `(path, method)` → `endpoint` map from the live FastAPI app,
/// then resolves each [`RouteManifest`] to a [`BoundRoute`].
pub fn bind_routes(
    py: Python<'_>,
    manifest: &AppManifest,
    app: &Bound<'_, PyAny>,
) -> Result<Vec<BoundRoute>, DiscoveryError> {
    let routes_obj = app
        .getattr(c"routes")
        .map_err(|e| DiscoveryError::Python(format!("get routes: {e}")))?;

    let routes_list = routes_obj
        .cast::<pyo3::types::PyList>()
        .map_err(|e| DiscoveryError::Python(format!("routes is not a list: {e}")))?;

    let api_route_cls = py
        .import(c"fastapi.routing")
        .and_then(|m| m.getattr(c"APIRoute"))
        .map_err(|e| DiscoveryError::Python(format!("import APIRoute: {e}")))?;

    let ws_route_cls = py
        .import(c"fastapi.routing")
        .and_then(|m| m.getattr(c"APIWebSocketRoute"))
        .ok();

    // Build endpoint map: (path, METHOD) → resolved endpoint.
    let mut endpoint_map: HashMap<(String, String), ResolvedEndpoint> = HashMap::new();
    // Build WS endpoint map: path → route object (the route itself is an ASGI app).
    let mut ws_endpoint_map: HashMap<String, Py<PyAny>> = HashMap::new();
    for route in routes_list {
        if route.is_instance(&api_route_cls).unwrap_or(false) {
            let path: String = route
                .getattr(c"path")
                .and_then(|v: Bound<'_, PyAny>| v.extract())
                .map_err(|e| DiscoveryError::Python(format!("route.path: {e}")))?;
            let methods: HashSet<String> = route
                .getattr(c"methods")
                .and_then(|v: Bound<'_, PyAny>| v.extract())
                .unwrap_or_default();
            let endpoint = route
                .getattr(c"endpoint")
                .map_err(|e| DiscoveryError::Python(format!("route.endpoint: {e}")))?
                .unbind();
            let response_model = route
                .getattr(c"response_model")
                .ok()
                .filter(|v| !v.is_none())
                .map(|v| v.unbind());
            for m in methods {
                endpoint_map.insert(
                    (path.clone(), m),
                    ResolvedEndpoint {
                        handler: endpoint.clone_ref(py),
                        response_model: response_model.as_ref().map(|r| r.clone_ref(py)),
                    },
                );
            }
        } else if let Some(ref ws_cls) = ws_route_cls
            && route.is_instance(ws_cls).unwrap_or(false)
        {
            let path: String = route
                .getattr(c"path")
                .and_then(|v: Bound<'_, PyAny>| v.extract())
                .map_err(|e| DiscoveryError::Python(format!("ws route.path: {e}")))?;
            ws_endpoint_map.insert(path, route.clone().unbind());
        }
    }

    let app_ref = App::new(app.clone().unbind());
    let mut bound = Vec::with_capacity(manifest.routes.len());

    for rm in &manifest.routes {
        if rm.kind == HandlerKind::WebSocket {
            // WS routes: look up the route object from ws_endpoint_map.
            // The APIWebSocketRoute itself is an ASGI app that accepts (scope, receive, send).
            let handler_obj = ws_endpoint_map
                .get(rm.path.as_str())
                .ok_or_else(|| {
                    DiscoveryError::InvalidRoute(format!("ws handler not found for {}", rm.path))
                })?
                .clone_ref(py);
            let handler = Handler::new(py, handler_obj);

            bound.push(BoundRoute {
                manifest: rm.clone(),
                handler,
                fastapi_app: Some(app_ref.clone_ref(py)),
                direct_context: None,
            });
            continue;
        }

        let method_str = rm.method.as_str();
        let key = (rm.path.as_str().to_owned(), method_str.to_owned());
        let endpoint = endpoint_map.get(&key).ok_or_else(|| {
            DiscoveryError::InvalidRoute(format!("handler not found for {method_str} {}", rm.path))
        })?;
        let handler_obj = endpoint.handler.clone_ref(py);
        let response_model = endpoint.response_model.as_ref().map(|r| r.clone_ref(py));
        let handler = Handler::new(py, handler_obj);

        let (direct_context, dispatch_override) =
            if rm.dispatch_strategy == DispatchStrategy::Direct {
                match build_direct_context(py, rm, response_model.as_ref()) {
                    Ok(ctx) => (Some(ctx), None),
                    Err(e) => {
                        tracing::warn!(
                            path = %rm.path,
                            error = %e,
                            "cannot build DirectContext, falling back to AsgiBridge"
                        );
                        (None, Some(DispatchStrategy::AsgiBridge))
                    }
                }
            } else {
                (None, None)
            };

        let mut manifest = rm.clone();
        if let Some(strategy) = dispatch_override {
            manifest.dispatch_strategy = strategy;
        }

        bound.push(BoundRoute {
            manifest,
            handler,
            fastapi_app: Some(app_ref.clone_ref(py)),
            direct_context,
        });
    }

    Ok(bound)
}

/// Bind manifest routes to live Python handlers, importing the FastAPI app for ASGI dispatch.
///
/// For each route, imports the handler by its dotted qualified name and builds
/// an endpoint map from the live FastAPI app so that `fastapi_app` is set on
/// all bound routes — enabling proper ASGI dispatch through FastAPI middleware.
pub fn bind_routes_from_manifest(
    py: Python<'_>,
    manifest: &AppManifest,
    app_module: &crate::route::AppModule,
) -> Result<Vec<BoundRoute>, DiscoveryError> {
    let app = super::fastapi::find_fastapi_app(py, app_module)?;
    bind_routes(py, manifest, &app)
}

/// Import a Python object by its dotted qualified name.
///
/// Splits on the last `.` to get `(module_path, attr_name)`, imports the
/// module, and returns the attribute.
///
/// # Errors
///
/// Returns `InvalidRoute` if `qualname` has no dot, or `Python` if the
/// module cannot be imported or the attribute is missing.
pub fn import_qualified_name(py: Python<'_>, qualname: &str) -> Result<Py<PyAny>, DiscoveryError> {
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

/// Build [`DirectContext`] for a route with [`DispatchStrategy::Direct`].
///
/// Resolves Pydantic model classes for body params, builds a `TypeAdapter`
/// for the response model (if any), and caches `json.dumps` and
/// `HTTPException` for use during direct dispatch.
fn build_direct_context(
    py: Python<'_>,
    rm: &RouteManifest,
    response_model: Option<&Py<PyAny>>,
) -> Result<DirectContext, DiscoveryError> {
    let body_validators = resolve_body_validators(py, rm)?;
    let response_adapter = resolve_response_adapter(py, rm, response_model)?;
    let json_dumps = py
        .import(c"json")
        .and_then(|m| m.getattr(c"dumps"))
        .map_err(|e| DiscoveryError::Python(format!("import json.dumps: {e}")))?
        .unbind();
    let http_exception_cls = py
        .import(c"fastapi.exceptions")
        .and_then(|m| m.getattr(c"HTTPException"))
        .map_err(|e| DiscoveryError::Python(format!("import HTTPException: {e}")))?
        .unbind();

    Ok(DirectContext {
        response_adapter,
        body_validators,
        json_dumps,
        http_exception_cls,
    })
}

/// Import Pydantic model classes for each body param.
fn resolve_body_validators(
    py: Python<'_>,
    rm: &RouteManifest,
) -> Result<Vec<(String, Py<PyAny>)>, DiscoveryError> {
    let mut validators = Vec::new();
    for param in &rm.params {
        if param.source == ParamSource::Body {
            let cls = import_qualified_name(py, param.type_qualname.as_str())?;
            validators.push((param.name.clone(), cls));
        }
    }
    Ok(validators)
}

/// Build a `pydantic.TypeAdapter` for the response model, if the route has one.
fn resolve_response_adapter(
    py: Python<'_>,
    rm: &RouteManifest,
    response_model: Option<&Py<PyAny>>,
) -> Result<Option<Py<PyAny>>, DiscoveryError> {
    let ResponseType::Model { ref qualname, .. } = rm.response_type else {
        return Ok(None);
    };

    // Resolve the model class: use the live response_model if available,
    // otherwise fall back to importing by qualname. Builtins with no
    // module component (e.g. `dict`, `list`) can only use the live object.
    let model_cls = if let Some(live) = response_model {
        live.clone_ref(py)
    } else if qualname.as_str().contains('.') {
        import_qualified_name(py, qualname.as_str())?
    } else {
        // No live object and no module path — use json.dumps path.
        return Ok(None);
    };

    let type_adapter_cls = py
        .import(c"pydantic")
        .and_then(|m| m.getattr(c"TypeAdapter"))
        .map_err(|e| DiscoveryError::Python(format!("import pydantic.TypeAdapter: {e}")))?;
    let adapter = type_adapter_cls
        .call1((model_cls,))
        .map_err(|e| DiscoveryError::Python(format!("build TypeAdapter: {e}")))?;
    Ok(Some(adapter.unbind()))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::with_py;

    #[test]
    fn import_qualified_name_builtin() {
        with_py(|py| {
            let result = import_qualified_name(py, "builtins.len");
            assert!(result.is_ok());
            // Verify it's callable
            assert!(result.unwrap().bind(py).is_callable());
        });
    }

    #[test]
    fn import_qualified_name_no_dot() {
        with_py(|py| {
            let result = import_qualified_name(py, "nodot");
            assert!(result.is_err());
            assert!(matches!(
                result.unwrap_err(),
                DiscoveryError::InvalidRoute(_)
            ));
        });
    }

    #[test]
    fn import_qualified_name_bad_module() {
        with_py(|py| {
            let result = import_qualified_name(py, "nonexistent_module_xyz.Thing");
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), DiscoveryError::Python(_)));
        });
    }

    #[test]
    fn import_qualified_name_bad_attr() {
        with_py(|py| {
            let result = import_qualified_name(py, "builtins.nonexistent_attr_xyz");
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), DiscoveryError::Python(_)));
        });
    }
}
