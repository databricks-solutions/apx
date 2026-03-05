//! Manifest → `BoundRoute` binding: import qualnames, resolve types.

use crate::discovery::DiscoveryError;
use crate::route::{AppManifest, BoundParam, BoundRoute, ParamManifest, ParamSource, ResponseType};
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

    // Build endpoint map: (path, METHOD) → handler callable.
    let mut endpoint_map: HashMap<(String, String), Py<PyAny>> = HashMap::new();
    for route in routes_list {
        if !route.is_instance(&api_route_cls).unwrap_or(false) {
            continue;
        }
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
    }

    let mut bound = Vec::with_capacity(manifest.routes.len());

    for rm in &manifest.routes {
        let method_str = rm.method.as_str();
        let key = (rm.path.as_str().to_owned(), method_str.to_owned());
        let handler = endpoint_map
            .get(&key)
            .ok_or_else(|| {
                DiscoveryError::InvalidRoute(format!(
                    "handler not found for {method_str} {}",
                    rm.path
                ))
            })?
            .clone_ref(py);

        let params = bind_params(py, &rm.params)?;
        let response_model = bind_response_model(py, &rm.response_type)?;
        let has_body_param = rm
            .params
            .iter()
            .any(|p| matches!(p.source, ParamSource::Body | ParamSource::RawBody));

        bound.push(BoundRoute {
            manifest: rm.clone(),
            handler,
            params,
            response_model,
            has_body_param,
            dependant: None,
            fastapi_app: None,
        });
    }

    Ok(bound)
}

/// Bind parameter manifests to their resolved Python types.
fn bind_params(
    py: Python<'_>,
    params: &[ParamManifest],
) -> Result<Vec<BoundParam>, DiscoveryError> {
    let mut bound = Vec::with_capacity(params.len());

    for param in params {
        let python_type = match param.source {
            ParamSource::Body => {
                let cls = import_qualified_name(py, param.type_qualname.as_str())?;
                Some(cls)
            }
            _ => None,
        };
        bound.push(BoundParam {
            manifest: param.clone(),
            python_type,
        });
    }

    Ok(bound)
}

/// Resolve the response model class for type checking.
fn bind_response_model(
    py: Python<'_>,
    response_type: &ResponseType,
) -> Result<Option<Py<PyAny>>, DiscoveryError> {
    match response_type {
        ResponseType::Model { qualname, .. } => {
            let cls = import_qualified_name(py, qualname.as_str())?;
            Ok(Some(cls))
        }
        ResponseType::StreamingResponse | ResponseType::RawResponse => Ok(None),
    }
}

/// Bind manifest routes to live Python handlers without importing FastAPI.
///
/// For each route, imports the handler by its dotted qualified name, resolves
/// Body param types and response model classes. No FastAPI app needed.
pub fn bind_routes_from_manifest(
    py: Python<'_>,
    manifest: &AppManifest,
) -> Result<Vec<BoundRoute>, DiscoveryError> {
    let mut bound = Vec::with_capacity(manifest.routes.len());

    for rm in &manifest.routes {
        let handler = import_qualified_name(py, rm.handler_qualname.as_str())?;
        let params = bind_params(py, &rm.params)?;
        let response_model = bind_response_model(py, &rm.response_type)?;
        let has_body_param = rm
            .params
            .iter()
            .any(|p| matches!(p.source, ParamSource::Body | ParamSource::RawBody));

        bound.push(BoundRoute {
            manifest: rm.clone(),
            handler,
            params,
            response_model,
            has_body_param,
            dependant: None,
            fastapi_app: None,
        });
    }

    Ok(bound)
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
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
