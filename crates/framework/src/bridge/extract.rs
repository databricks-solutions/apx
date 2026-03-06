//! Shared HTTP parameter extraction logic.
//!
//! Used by both `dispatch.rs` (direct dispatch) and `plan_executor.rs`
//! (compiled plan execution). Functions return `Bound<'py, PyAny>` —
//! callers `.unbind()` if needed.

use crate::error::{AppError, ValidationErrorItem};
use pyo3::Python;
use pyo3::types::{PyAnyMethods, PyString};

/// Scalar type conversion for path/query/header/cookie parameters.
enum ScalarConversion {
    Int,
    Float,
    Str,
}

impl ScalarConversion {
    fn from_type_name(name: &str) -> Self {
        match name {
            "int" => Self::Int,
            "float" => Self::Float,
            _ => Self::Str,
        }
    }

    fn convert<'py>(
        &self,
        py: Python<'py>,
        value: &str,
    ) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
        let py_str = PyString::new(py, value);
        match self {
            Self::Str => Ok(py_str.into_any()),
            conv => {
                let builtins = py
                    .import(c"builtins")
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                let (func_name, type_label) = match conv {
                    Self::Int => (c"int", "integer"),
                    Self::Float => (c"float", "float"),
                    Self::Str => unreachable!(),
                };
                builtins
                    .getattr(func_name)
                    .and_then(|f| f.call1((py_str,)))
                    .map_err(|_| {
                        AppError::BadRequest(format!(
                            "path param is not a valid {type_label}: {value}"
                        ))
                    })
            }
        }
    }
}

/// Convert a string value to the target Python scalar type.
pub(super) fn convert_scalar<'py>(
    py: Python<'py>,
    value: &str,
    type_name: &str,
) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
    ScalarConversion::from_type_name(type_name).convert(py, value)
}

/// Extract a path parameter, converting to the target type.
pub(super) fn extract_path_value<'py>(
    py: Python<'py>,
    path_params: &[(String, String)],
    name: &str,
    type_name: &str,
    required: bool,
) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
    let raw = path_params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str());

    match raw {
        Some(v) => convert_scalar(py, v, type_name),
        None if !required => Ok(py.None().into_bound(py)),
        None => Err(AppError::BadRequest(format!(
            "missing path parameter: {name}"
        ))),
    }
}

/// Extract a query parameter, converting to the target type.
pub(super) fn extract_query_value<'py>(
    py: Python<'py>,
    query_params: &[(String, String)],
    name: &str,
    type_name: &str,
    required: bool,
    default_json: Option<&serde_json::Value>,
) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
    let raw = query_params
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str());

    match raw {
        Some(v) => convert_scalar(py, v, type_name),
        None if !required => resolve_default(py, default_json).map(|v| v.into_bound(py)),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["query".to_owned(), name.to_owned()],
            msg: "Field required".to_owned(),
            r#type: "missing".to_owned(),
        }])),
    }
}

/// Extract a header parameter by wire name.
pub(super) fn extract_header_value<'py>(
    py: Python<'py>,
    headers: &http::HeaderMap,
    wire_name: &str,
    type_name: &str,
    required: bool,
) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
    match headers.get(wire_name) {
        Some(value) => {
            let value_str = value.to_str().map_err(|_| {
                AppError::BadRequest(format!(
                    "header '{wire_name}' contains non-ASCII characters"
                ))
            })?;
            convert_scalar(py, value_str, type_name)
        }
        None if !required => Ok(py.None().into_bound(py)),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["header".into(), wire_name.into()],
            msg: format!("missing required header: {wire_name}"),
            r#type: "missing".into(),
        }])),
    }
}

/// Extract a cookie parameter by parsing the `Cookie` header.
pub(super) fn extract_cookie_value<'py>(
    py: Python<'py>,
    headers: &http::HeaderMap,
    name: &str,
    type_name: &str,
    required: bool,
) -> Result<pyo3::Bound<'py, pyo3::PyAny>, AppError> {
    let cookie_header = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let value = cookie_header.split(';').find_map(|pair| {
        let pair = pair.trim();
        pair.split_once('=')
            .filter(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim())
    });

    match value {
        Some(v) => convert_scalar(py, v, type_name),
        None if !required => Ok(py.None().into_bound(py)),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["cookie".into(), name.into()],
            msg: format!("missing required cookie: {name}"),
            r#type: "missing".into(),
        }])),
    }
}

/// Resolve a default value from JSON via `json.loads`, falling back to `None`.
pub(super) fn resolve_default(
    py: Python<'_>,
    default_json: Option<&serde_json::Value>,
) -> Result<pyo3::Py<pyo3::PyAny>, AppError> {
    let Some(value) = default_json else {
        return Ok(py.None());
    };
    if value.is_null() {
        return Ok(py.None());
    }
    let json_str = serde_json::to_string(value)
        .map_err(|e| AppError::Internal(format!("serialize default: {e}")))?;
    let json_mod = py
        .import(c"json")
        .map_err(|e| AppError::Internal(format!("import json: {e}")))?;
    json_mod
        .call_method1(c"loads", (json_str.as_str(),))
        .map(|v| v.unbind())
        .map_err(|e| AppError::Internal(format!("json.loads(default): {e}")))
}
