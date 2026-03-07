//! Serializable manifest types — no PyO3 dependency.
//!
//! These types form the build artifact produced by `apx build` and consumed
//! by `apx serve`. They are pure data with serde derives.

use super::dependency::{DependencyNode, DependencyPlan, LifecycleDepManifest};
use super::primitives::{AppModule, BodyLimit, QualName, RoutePath};
use serde::{Deserialize, Serialize};
use std::fmt;

// ── HTTP method ─────────────────────────────────────────────────────────

/// HTTP method (mirrors the Python `HttpMethod` enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HttpMethod {
    /// GET
    Get,
    /// POST
    Post,
    /// PUT
    Put,
    /// DELETE
    Delete,
    /// PATCH
    Patch,
}

impl HttpMethod {
    /// Uppercase HTTP method string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
        }
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Handler kind ────────────────────────────────────────────────────────

/// What kind of handler this route uses.
///
/// Determines which [`HandlerDispatch`](crate::bridge::dispatch::HandlerDispatch)
/// implementation is selected at router construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandlerKind {
    /// Standard request → response.
    RequestResponse,
    /// Server-sent events — returns `StreamingResponse` with `text/event-stream`.
    SSE,
    /// WebSocket route via `APIWebSocketRoute`.
    WebSocket,
}

// ── Parameter metadata ──────────────────────────────────────────────────

/// Where a handler parameter comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamSource {
    /// From URL path template variable: `/items/{item_id}`.
    Path,
    /// From URL query string: `?key=value`.
    Query,
    /// From an HTTP header via FastAPI `Header()`.
    Header,
    /// From an HTTP cookie via FastAPI `Cookie()`.
    Cookie,
    /// From request body (JSON), validated via Pydantic.
    Body,
    /// Raw request body bytes — no JSON parsing, no Pydantic validation.
    RawBody,
}

/// Parameter metadata — serializable, uses qualified type name not `Py<PyAny>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamManifest {
    /// Parameter name from the Python function signature.
    pub name: String,
    /// Where this parameter's value comes from.
    pub source: ParamSource,
    /// Qualified Python type name: `QualName("int")`, `QualName("backend.app.ItemCreate")`.
    pub type_qualname: QualName,
    /// Whether the parameter is required (no default value).
    pub required: bool,
    /// JSON Schema for this param's type (populated by `apx build`, used for OpenAPI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    /// Wire name (e.g. `"x-token"` for header, differs from Python name `"x_token"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Serialized default value (JSON), if the param has a default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_json: Option<serde_json::Value>,
}

// ── Response type ───────────────────────────────────────────────────────

/// What the handler returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseType {
    /// Pydantic `ResponseModel` subclass (camelCase serialization).
    Model {
        /// Qualified name of the response model class.
        qualname: QualName,
        /// JSON Schema (populated by `apx build`, used for OpenAPI).
        #[serde(skip_serializing_if = "Option::is_none")]
        json_schema: Option<serde_json::Value>,
        /// HTTP status code (default 200).
        status_code: u16,
    },
    /// SSE or chunked streaming response.
    StreamingResponse,
    /// Raw `apx.Response`.
    RawResponse,
}

impl fmt::Display for ResponseType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model { qualname, .. } => write!(f, "Model({qualname})"),
            Self::StreamingResponse => f.write_str("StreamingResponse"),
            Self::RawResponse => f.write_str("RawResponse"),
        }
    }
}

// ── Build metadata ──────────────────────────────────────────────────────

/// Build metadata — for version checks and staleness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestMeta {
    /// apx version that produced this manifest.
    pub apx_version: String,
    /// Python interpreter version.
    pub python_version: String,
    /// FastAPI version (if installed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fastapi_version: Option<String>,
    /// ISO 8601 build timestamp.
    pub build_timestamp: String,
    /// Application module path.
    pub app_module: AppModule,
    /// SHA-256 of all `.py` files in the app module (for staleness detection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
}

/// Validation check performed at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    /// Check name (e.g. `"no_cycles"`, `"valid_types"`).
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Detail message (for failures).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ── Route & app manifests ───────────────────────────────────────────────

/// Route metadata — everything known at build time. No `Py<PyAny>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteManifest {
    /// Handler kind (request-response, SSE, etc.).
    pub kind: HandlerKind,
    /// HTTP method.
    pub method: HttpMethod,
    /// URL path template.
    pub path: RoutePath,
    /// Python qualified name of the handler function.
    pub handler_qualname: QualName,
    /// Handler parameters.
    pub params: Vec<ParamManifest>,
    /// Return type information.
    pub response_type: ResponseType,
    /// Route tags (for grouping in OpenAPI docs).
    pub tags: Vec<String>,
    /// Pre-compiled dependency execution plan (manifest mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_plan: Option<DependencyPlan>,
    /// HTTP status code for this route (from FastAPI decorator or response_model).
    pub status_code: u16,
    /// Route summary (from docstring or explicit, used in OpenAPI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Route description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether to include in OpenAPI schema.
    pub include_in_schema: bool,
    /// Deprecated flag.
    pub deprecated: bool,
    /// OpenAPI operation ID (if explicitly set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Whether the handler is an async function.
    /// Sync handlers run on the blocking threadpool instead of the event loop.
    #[serde(default = "default_true")]
    pub is_async_handler: bool,
}

fn default_true() -> bool {
    true
}

/// Full build artifact — all routes plus app config.
///
/// Produced by `apx build`, loaded by `apx serve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    /// Build metadata (version checks, staleness detection).
    /// `None` in dev mode (live discovery), `Some` when loaded from a manifest file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ManifestMeta>,
    /// All registered routes.
    pub routes: Vec<RouteManifest>,
    /// App-wide dependency graph.
    pub dependency_graph: Vec<DependencyNode>,
    /// Lifecycle dependencies (resolved once per worker).
    pub lifecycle_deps: Vec<LifecycleDepManifest>,
    /// Pre-generated OpenAPI 3.1 schema (from `app.openapi()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openapi_schema: Option<serde_json::Value>,
    /// Max request body size.
    pub max_body_limit: BodyLimit,
    /// Build-time validation results.
    pub validation_results: Vec<ValidationCheck>,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::super::primitives::{AppModule, BodyLimit, QualName, RoutePath};
    use super::*;

    #[test]
    fn param_source_serde_roundtrip() {
        let variants = [
            ParamSource::Path,
            ParamSource::Query,
            ParamSource::Header,
            ParamSource::Cookie,
            ParamSource::Body,
            ParamSource::RawBody,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap_or_default();
            let back: ParamSource = serde_json::from_str(&json).unwrap_or(ParamSource::Path);
            assert_eq!(v, back, "roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn handler_kind_serde() {
        for kind in [
            HandlerKind::RequestResponse,
            HandlerKind::SSE,
            HandlerKind::WebSocket,
        ] {
            let json = serde_json::to_string(&kind).unwrap_or_default();
            let back: HandlerKind =
                serde_json::from_str(&json).unwrap_or(HandlerKind::RequestResponse);
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn http_method_serde_all_variants() {
        for method in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Delete,
            HttpMethod::Patch,
        ] {
            let json = serde_json::to_string(&method).unwrap();
            let back: HttpMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(method, back);
        }
    }

    #[test]
    fn manifest_meta_serde() {
        let meta = ManifestMeta {
            apx_version: "0.3.8".to_owned(),
            python_version: "3.11.0".to_owned(),
            fastapi_version: Some("0.115.0".to_owned()),
            build_timestamp: "2024-01-01T00:00:00Z".to_owned(),
            app_module: AppModule::new("backend.app").unwrap_or_else(|_| unreachable!()),
            source_hash: None,
        };
        let json = serde_json::to_string(&meta).unwrap_or_default();
        let back: ManifestMeta = serde_json::from_str(&json).unwrap_or_else(|_| ManifestMeta {
            apx_version: String::new(),
            python_version: String::new(),
            fastapi_version: None,
            build_timestamp: String::new(),
            app_module: AppModule::new("x").unwrap_or_else(|_| unreachable!()),
            source_hash: None,
        });
        assert_eq!(back.apx_version, "0.3.8");
    }

    #[test]
    fn app_manifest_serde_roundtrip() {
        let manifest = AppManifest {
            meta: None,
            routes: vec![RouteManifest {
                kind: HandlerKind::RequestResponse,
                method: HttpMethod::Get,
                path: RoutePath::new("/items").unwrap_or_else(|_| unreachable!()),
                handler_qualname: QualName::new("backend.app.list_items")
                    .unwrap_or_else(|_| unreachable!()),
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
            }],
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
        };
        let json = serde_json::to_string(&manifest).unwrap_or_default();
        let back: AppManifest = serde_json::from_str(&json).unwrap_or_else(|_| AppManifest {
            meta: None,
            routes: Vec::new(),
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
        });
        assert_eq!(back.routes.len(), 1);
        assert_eq!(back.routes[0].path.as_str(), "/items");
    }

    #[test]
    fn response_type_display_model() {
        let rt = ResponseType::Model {
            qualname: QualName::new("backend.Item").unwrap(),
            json_schema: None,
            status_code: 200,
        };
        assert_eq!(format!("{rt}"), "Model(backend.Item)");
    }

    #[test]
    fn response_type_display_streaming() {
        assert_eq!(
            format!("{}", ResponseType::StreamingResponse),
            "StreamingResponse"
        );
    }

    #[test]
    fn response_type_display_raw() {
        assert_eq!(format!("{}", ResponseType::RawResponse), "RawResponse");
    }

    #[test]
    fn response_type_serde_model_with_schema() {
        let rt = ResponseType::Model {
            qualname: QualName::new("backend.Item").unwrap(),
            json_schema: Some(serde_json::json!({"type": "object"})),
            status_code: 201,
        };
        let json = serde_json::to_string(&rt).unwrap();
        let back: ResponseType = serde_json::from_str(&json).unwrap();
        match back {
            ResponseType::Model {
                qualname,
                json_schema,
                status_code,
            } => {
                assert_eq!(qualname.as_str(), "backend.Item");
                assert!(json_schema.is_some());
                assert_eq!(status_code, 201);
            }
            _ => panic!("expected Model"),
        }
    }

    #[test]
    fn response_type_serde_streaming() {
        let rt = ResponseType::StreamingResponse;
        let json = serde_json::to_string(&rt).unwrap();
        let back: ResponseType = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ResponseType::StreamingResponse));
    }

    #[test]
    fn validation_check_serde_passed() {
        let check = ValidationCheck {
            name: "no_cycles".to_owned(),
            passed: true,
            detail: None,
        };
        let json = serde_json::to_string(&check).unwrap();
        let back: ValidationCheck = serde_json::from_str(&json).unwrap();
        assert!(back.passed);
        assert!(back.detail.is_none());
    }

    #[test]
    fn validation_check_serde_failed() {
        let check = ValidationCheck {
            name: "valid_types".to_owned(),
            passed: false,
            detail: Some("type 'Foo' not found".to_owned()),
        };
        let json = serde_json::to_string(&check).unwrap();
        let back: ValidationCheck = serde_json::from_str(&json).unwrap();
        assert!(!back.passed);
        assert_eq!(back.detail.as_deref(), Some("type 'Foo' not found"));
    }
}
