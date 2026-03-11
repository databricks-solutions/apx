//! Integration tests for direct dispatch — requires fastapi in the venv.

use crate::bridge::direct_dispatch::{classify_handler_error, serialize_response};
use crate::error::AppError;
use crate::route::{DirectContext, ResponseType};
use crate::with_py;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::ensure_python_env;

/// Build a [`DirectContext`] with fastapi from the venv.
fn make_ctx(py: Python<'_>) -> DirectContext {
    let json_dumps = py
        .import(c"json")
        .unwrap()
        .getattr(c"dumps")
        .unwrap()
        .unbind();
    let http_exc_cls = py
        .import(c"fastapi.exceptions")
        .unwrap()
        .getattr(c"HTTPException")
        .unwrap()
        .unbind();
    DirectContext {
        response_adapter: None,
        body_validators: Vec::new(),
        json_dumps,
        http_exception_cls: http_exc_cls,
    }
}

// ── classify_handler_error ──────────────────────────────────────────────

#[test]
fn classify_handler_error_generic_exception() {
    ensure_python_env();
    with_py(|py| {
        let ctx = make_ctx(py);
        let err = PyErr::new::<pyo3::exceptions::PyValueError, _>("boom");
        let classified = classify_handler_error(py, &err, &ctx);
        assert!(matches!(classified, AppError::Internal(_)));
    });
}

#[test]
fn classify_handler_error_http_exception_404() {
    ensure_python_env();
    with_py(|py| {
        let ctx = make_ctx(py);
        let exc_instance = ctx
            .http_exception_cls
            .call1(py, (404, "Not found"))
            .unwrap();
        let err = PyErr::from_value(exc_instance.into_bound(py));
        let classified = classify_handler_error(py, &err, &ctx);
        match classified {
            AppError::HttpException { status, detail } => {
                assert_eq!(status, 404);
                assert_eq!(detail, "Not found");
            }
            other => panic!("expected HttpException, got {other:?}"),
        }
    });
}

// ── serialize_response ──────────────────────────────────────────────────

#[test]
fn serialize_response_dict() {
    ensure_python_env();
    with_py(|py| {
        let ctx = make_ctx(py);
        let dict = PyDict::new(py);
        dict.set_item("echo", true).unwrap();
        let (status, body) =
            serialize_response(py, &dict.into_any(), &ctx, &ResponseType::RawResponse).unwrap();
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["echo"], true);
    });
}

#[test]
fn serialize_response_none_204() {
    ensure_python_env();
    with_py(|py| {
        let ctx = make_ctx(py);
        let none = py.None().into_bound(py);
        let (status, body) =
            serialize_response(py, &none, &ctx, &ResponseType::RawResponse).unwrap();
        assert_eq!(status, 204);
        assert!(body.is_empty());
    });
}
