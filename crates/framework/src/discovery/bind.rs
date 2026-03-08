//! Manifest → `BoundRoute` binding: import qualnames, resolve types.

use crate::discovery::DiscoveryError;
use crate::route::{App, AppManifest, BoundRoute, Handler, HandlerKind};
use pyo3::types::{PyAnyMethods, PyString};
use pyo3::{Bound, Py, PyAny, Python};
use std::collections::{HashMap, HashSet};

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

    // Build endpoint map: (path, METHOD) → handler callable.
    let mut endpoint_map: HashMap<(String, String), Py<PyAny>> = HashMap::new();
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
            for m in methods {
                endpoint_map.insert((path.clone(), m), endpoint.clone_ref(py));
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
            });
            continue;
        }

        let method_str = rm.method.as_str();
        let key = (rm.path.as_str().to_owned(), method_str.to_owned());
        let handler_obj = endpoint_map
            .get(&key)
            .ok_or_else(|| {
                DiscoveryError::InvalidRoute(format!(
                    "handler not found for {method_str} {}",
                    rm.path
                ))
            })?
            .clone_ref(py);
        let handler = Handler::new(py, handler_obj);

        bound.push(BoundRoute {
            manifest: rm.clone(),
            handler,
            fastapi_app: Some(app_ref.clone_ref(py)),
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
