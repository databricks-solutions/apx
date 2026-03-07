//! Route manifest and bound types.
//!
//! Manifest types are serializable (no PyO3) — they form the build artifact
//! produced by `apx build` and consumed by `apx serve`.
//!
//! Bound types carry live Python objects and are constructed at runtime during
//! route discovery.

use pyo3::types::{PyAny, PyAnyMethods};
use pyo3::{Py, Python};
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Domain newtypes ─────────────────────────────────────────────────────

/// Validate a single segment of a Python dotted path.
///
/// A valid segment is a Python identifier: non-empty, does not start with
/// an ASCII digit, and contains only alphanumeric characters or underscores.
fn validate_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with(|c: char| c.is_ascii_digit())
        && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Python qualified name: `"backend.app.create_item"`, `"int"`, `"str"`.
///
/// Used for handler references, parameter type identification, and OpenAPI
/// schema generation. Each dot-separated segment must be a valid Python
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualName(String);

/// Validation errors for [`QualName`].
#[derive(Debug, thiserror::Error)]
pub enum QualNameError {
    /// The name was empty.
    #[error("qualified name must not be empty")]
    Empty,
    /// A segment between dots was empty (e.g. `"foo..bar"`).
    #[error("qualified name has empty segment: {0}")]
    EmptySegment(String),
    /// A segment is not a valid Python identifier.
    #[error("invalid qualified name segment: {0}")]
    InvalidSegment(String),
}

impl QualName {
    /// Create a new qualified name, validating all segments.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty or any segment is invalid.
    pub fn new(name: impl Into<String>) -> Result<Self, QualNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(QualNameError::Empty);
        }
        for segment in name.split('.') {
            if segment.is_empty() {
                return Err(QualNameError::EmptySegment(name));
            }
            if !validate_segment(segment) {
                return Err(QualNameError::InvalidSegment(segment.to_owned()));
            }
        }
        Ok(Self(name))
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QualName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Python module path: `"backend.app"`, `"mypackage.api"`.
///
/// Must be a valid Python dotted path. Format validation only — runtime
/// validation (module importable, contains App instance) happens during
/// worker discovery.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppModule(String);

/// Validation errors for [`AppModule`].
#[derive(Debug, thiserror::Error)]
pub enum AppModuleError {
    /// The module path was empty.
    #[error("app module must not be empty")]
    Empty,
    /// A segment between dots was empty.
    #[error("app module has empty segment: {0}")]
    EmptySegment(String),
    /// A segment is not a valid Python identifier.
    #[error("invalid module segment: {0}")]
    InvalidSegment(String),
}

impl AppModule {
    /// Create a new module path, validating all segments.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is empty or any segment is invalid.
    pub fn new(module: impl Into<String>) -> Result<Self, AppModuleError> {
        let module = module.into();
        if module.is_empty() {
            return Err(AppModuleError::Empty);
        }
        for segment in module.split('.') {
            if segment.is_empty() {
                return Err(AppModuleError::EmptySegment(module));
            }
            if !validate_segment(segment) {
                return Err(AppModuleError::InvalidSegment(segment.to_owned()));
            }
        }
        Ok(Self(module))
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated route path template: `"/items/{item_id}"`.
///
/// Must start with `'/'`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoutePath(String);

/// Validation errors for [`RoutePath`].
#[derive(Debug, thiserror::Error)]
pub enum RoutePathError {
    /// Path did not start with `/`.
    #[error("route path must start with '/', got: {0}")]
    MissingLeadingSlash(String),
}

impl RoutePath {
    /// Create a new route path, validating the leading slash.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not start with `/`.
    pub fn new(path: impl Into<String>) -> Result<Self, RoutePathError> {
        let path = path.into();
        if !path.starts_with('/') {
            return Err(RoutePathError::MissingLeadingSlash(path));
        }
        Ok(Self(path))
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoutePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Max request body size in bytes.
///
/// Newtype prevents mixing with other `usize` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyLimit(pub usize);

impl BodyLimit {
    /// Default body limit: 1 MiB.
    pub const DEFAULT: Self = Self(1024 * 1024);
}

/// RFC 9457 problem type URI reference.
///
/// Must be `"about:blank"` or contain a URI scheme (e.g., `"https://"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemTypeUri(String);

/// Validation error for [`ProblemTypeUri`].
#[derive(Debug, thiserror::Error)]
#[error("problem type must be 'about:blank' or a URI with scheme, got: {0}")]
pub struct ProblemTypeUriError(String);

impl ProblemTypeUri {
    /// The blank problem type per RFC 9457.
    pub const BLANK: &str = "about:blank";

    /// Create a new problem type URI.
    ///
    /// # Errors
    ///
    /// Returns an error if the URI is neither `"about:blank"` nor contains `"://"`.
    pub fn new(uri: impl Into<String>) -> Result<Self, ProblemTypeUriError> {
        let uri = uri.into();
        if uri == Self::BLANK || uri.contains("://") {
            Ok(Self(uri))
        } else {
            Err(ProblemTypeUriError(uri))
        }
    }

    /// Create the blank problem type.
    #[must_use]
    pub fn blank() -> Self {
        Self(Self::BLANK.to_owned())
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── Manifest types (serializable, no PyO3) ──────────────────────────────

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

// ── Dependency types ─────────────────────────────────────────────────────

/// A single step in the compiled dependency execution plan.
///
/// Steps are topologically sorted — can be executed sequentially.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependencyStep {
    /// Reserved for future native parameter design.
    /// Will be resolved in Rust with no Python call / no GIL.
    ResolveNative {
        /// Qualified name of the native dependency.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
        /// Configuration TBD — will be defined when native parameter design is finalized.
        config: serde_json::Value,
    },

    /// Resolved once per worker (lifecycle dep). Cached value injected per request.
    ResolveLifecycle {
        /// Qualified name of the lifecycle dependency.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
    },

    /// Call a Python function (standard `Depends`).
    /// Inputs are kwargs produced by earlier steps.
    CallPython {
        /// Qualified name of the dependency callable.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
        /// Names of kwargs this step needs from previous steps' outputs.
        inputs: Vec<String>,
        /// True if the function is an async generator (needs cleanup).
        is_generator: bool,
        /// True if the function is async.
        is_async: bool,
        /// FastAPI's `use_cache` dedup key.
        use_cache: bool,
    },

    /// Extract path param from axum's matched params. Rust-native.
    ExtractPath {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
    },

    /// Extract query param from URL. Rust-native.
    ExtractQuery {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the parameter is required.
        required: bool,
        /// Serialized default value (JSON).
        #[serde(skip_serializing_if = "Option::is_none")]
        default_json: Option<serde_json::Value>,
    },

    /// Extract header value. Rust-native.
    ExtractHeader {
        /// Parameter name.
        name: String,
        /// Wire name (lowercased, hyphenated): `"x-custom-token"`.
        alias: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the header is required.
        required: bool,
    },

    /// Extract cookie value. Rust-native.
    ExtractCookie {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the cookie is required.
        required: bool,
    },

    /// Parse + validate request body via Pydantic `model_validate_json`.
    ValidateBody {
        /// Parameter name.
        name: String,
        /// Pydantic model qualified name.
        model_qualname: QualName,
    },
}

/// Pre-compiled execution plan for a single route's dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyPlan {
    /// Topologically sorted steps — execute in order.
    pub steps: Vec<DependencyStep>,
    /// Final kwarg names to pass to the handler (in order).
    pub handler_kwargs: Vec<String>,
    /// Whether any step requires ASGI objects (Request, `solve_dependencies`, etc.).
    pub needs_asgi: bool,
    /// Generator steps that need cleanup after handler returns (indices into `steps`).
    pub generator_cleanup_indices: Vec<usize>,
}

/// Scope at which a dependency is instantiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepScope {
    /// Created once per worker, shared across all requests (e.g., DB engine).
    Worker,
    /// Created per request (e.g., DB session).
    Request,
}

/// Classification of a dependency for dispatch optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepTier {
    /// Reserved — native parameter design TBD.
    Native,
    /// Resolved per-worker or per-request via apx lifecycle.
    Lifecycle,
    /// Standard FastAPI `Depends()` — called per-request in Python.
    Standard,
}

/// A node in the app-wide dependency graph (manifest-level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyNode {
    /// Unique identifier: the Python qualname of the dep callable.
    pub qualname: QualName,
    /// How this dep is resolved.
    pub tier: DepTier,
    /// When this dep is instantiated.
    pub scope: DepScope,
    /// Whether the callable is an async generator (yields via context manager).
    pub is_generator: bool,
    /// Whether the callable is async.
    pub is_async: bool,
    /// Qualnames of dependencies this node depends on.
    pub sub_dependencies: Vec<QualName>,
    /// Parameters of the dep function itself (for validation).
    pub params: Vec<ParamManifest>,
}

/// A dependency resolved once per worker lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleDepManifest {
    /// Qualified name of the lifecycle dependency callable.
    pub qualname: QualName,
    /// Position in initialization order (topological sort).
    pub init_order: usize,
    /// Position in shutdown order (reverse of init).
    pub shutdown_order: usize,
    /// Scope of the dependency.
    pub scope: DepScope,
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

// ── Bound types (runtime, needs PyO3) ───────────────────────────────────

// ── Domain newtypes for Py<PyAny> fields ────────────────────────────────

/// A Python callable that handles an HTTP request.
///
/// Wraps the endpoint function discovered from a route.
/// Methods centralize all call-site Python interop.
pub(crate) struct Handler(Py<PyAny>);

impl Handler {
    /// Wrap a Python callable, asserting it's actually callable.
    pub(crate) fn new(py: Python<'_>, obj: Py<PyAny>) -> Self {
        debug_assert!(
            obj.bind(py).is_callable(),
            "Handler: object is not callable"
        );
        Self(obj)
    }

    /// Create a stub handler for unit tests (skips callable check).
    #[cfg(test)]
    pub(crate) fn stub(obj: Py<PyAny>) -> Self {
        Self(obj)
    }

    /// Borrow the inner reference (for ASGI bridge dispatch).
    pub(crate) fn inner(&self) -> &Py<PyAny> {
        &self.0
    }
}

impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Handler").field(&"<callable>").finish()
    }
}

/// The live ASGI application instance (for dependency_overrides, middleware).
pub(crate) struct App(Py<PyAny>);

impl App {
    pub(crate) fn new(obj: Py<PyAny>) -> Self {
        Self(obj)
    }

    /// Clone the reference (requires GIL).
    pub(crate) fn clone_ref(&self, py: Python<'_>) -> Self {
        Self(self.0.clone_ref(py))
    }

    /// Borrow the inner reference.
    pub(crate) fn inner(&self) -> &Py<PyAny> {
        &self.0
    }
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("App").field(&"<app>").finish()
    }
}

/// A route bound to its runtime implementation.
///
/// Shared via `Arc<BoundRoute>` — never cloned (`Py<PyAny>` fields are not
/// `Clone`-safe without the GIL).
///
/// Constructed in [`discovery`](crate::discovery), consumed in [`bridge`](crate::bridge).
pub(crate) struct BoundRoute {
    /// Serializable route metadata.
    pub(crate) manifest: RouteManifest,
    /// Python handler callable (for WS) or the ASGI app callable (for HTTP via FastAPI).
    pub(crate) handler: Handler,
    /// Reference to the live FastAPI app (for ASGI bridge dispatch).
    pub(crate) fastapi_app: Option<App>,
}

impl fmt::Debug for BoundRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundRoute")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn validate_segment_valid() {
        assert!(validate_segment("foo_bar"));
        assert!(validate_segment("_private"));
        assert!(validate_segment("CamelCase"));
    }

    #[test]
    fn validate_segment_empty() {
        assert!(!validate_segment(""));
    }

    #[test]
    fn validate_segment_leading_digit() {
        assert!(!validate_segment("1abc"));
    }

    #[test]
    fn validate_segment_hyphen() {
        assert!(!validate_segment("foo-bar"));
    }

    #[test]
    fn validate_segment_unicode() {
        // Unicode alphanumerics are accepted by `char::is_alphanumeric`.
        assert!(validate_segment("café"));
    }

    #[test]
    fn qualname_valid() {
        assert!(QualName::new("int").is_ok());
        assert!(QualName::new("backend.app.Item").is_ok());
    }

    #[test]
    fn qualname_empty() {
        assert!(matches!(QualName::new(""), Err(QualNameError::Empty)));
    }

    #[test]
    fn qualname_empty_segment() {
        assert!(matches!(
            QualName::new("foo..bar"),
            Err(QualNameError::EmptySegment(_))
        ));
    }

    #[test]
    fn qualname_invalid_segment() {
        assert!(matches!(
            QualName::new("foo.1bar"),
            Err(QualNameError::InvalidSegment(_))
        ));
    }

    #[test]
    fn app_module_valid() {
        assert!(AppModule::new("backend.app").is_ok());
        assert!(AppModule::new("app").is_ok());
    }

    #[test]
    fn app_module_empty() {
        assert!(matches!(AppModule::new(""), Err(AppModuleError::Empty)));
    }

    #[test]
    fn route_path_valid() {
        assert!(RoutePath::new("/items/{item_id}").is_ok());
        assert!(RoutePath::new("/").is_ok());
    }

    #[test]
    fn route_path_missing_slash() {
        assert!(matches!(
            RoutePath::new("items"),
            Err(RoutePathError::MissingLeadingSlash(_))
        ));
    }

    #[test]
    fn problem_type_uri_blank() {
        assert!(ProblemTypeUri::new("about:blank").is_ok());
    }

    #[test]
    fn problem_type_uri_https() {
        assert!(ProblemTypeUri::new("https://example.com/problems/not-found").is_ok());
    }

    #[test]
    fn problem_type_uri_invalid() {
        assert!(ProblemTypeUri::new("not-a-uri").is_err());
    }

    #[test]
    fn body_limit_default() {
        assert_eq!(BodyLimit::DEFAULT.0, 1024 * 1024);
    }

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
    fn dependency_step_serde_roundtrip() {
        let step = DependencyStep::CallPython {
            dep_qualname: QualName::new("backend.deps.get_db")
                .unwrap_or_else(|_| QualName::new("x").unwrap_or_else(|_| unreachable!())),
            target_kwarg: "db".to_owned(),
            inputs: vec!["session".to_owned()],
            is_generator: true,
            is_async: true,
            use_cache: false,
        };
        let json = serde_json::to_string(&step).unwrap_or_default();
        let back: DependencyStep =
            serde_json::from_str(&json).unwrap_or_else(|_| DependencyStep::ExtractPath {
                name: String::new(),
                type_qualname: QualName::new("str").unwrap_or_else(|_| unreachable!()),
            });
        assert!(matches!(back, DependencyStep::CallPython { .. }));
    }

    #[test]
    fn dependency_plan_serde_roundtrip() {
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractPath {
                name: "id".to_owned(),
                type_qualname: QualName::new("int").unwrap_or_else(|_| unreachable!()),
            }],
            handler_kwargs: vec!["id".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let json = serde_json::to_string(&plan).unwrap_or_default();
        let back: DependencyPlan = serde_json::from_str(&json).unwrap_or_else(|_| DependencyPlan {
            steps: Vec::new(),
            handler_kwargs: Vec::new(),
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        });
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.handler_kwargs, vec!["id"]);
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

    // ── DependencyStep serde roundtrips ──────────────────────────────────

    #[test]
    fn dependency_step_serde_resolve_native() {
        let step = DependencyStep::ResolveNative {
            dep_qualname: QualName::new("native.dep").unwrap(),
            target_kwarg: "dep".to_owned(),
            config: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ResolveNative { .. }));
    }

    #[test]
    fn dependency_step_serde_resolve_lifecycle() {
        let step = DependencyStep::ResolveLifecycle {
            dep_qualname: QualName::new("db.engine").unwrap(),
            target_kwarg: "engine".to_owned(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ResolveLifecycle { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_path() {
        let step = DependencyStep::ExtractPath {
            name: "item_id".to_owned(),
            type_qualname: QualName::new("int").unwrap(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractPath { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_query_with_default() {
        let step = DependencyStep::ExtractQuery {
            name: "page".to_owned(),
            type_qualname: QualName::new("int").unwrap(),
            required: false,
            default_json: Some(serde_json::json!(1)),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        match back {
            DependencyStep::ExtractQuery {
                default_json,
                required,
                ..
            } => {
                assert!(!required);
                assert!(default_json.is_some());
            }
            _ => panic!("expected ExtractQuery"),
        }
    }

    #[test]
    fn dependency_step_serde_extract_query_no_default() {
        let step = DependencyStep::ExtractQuery {
            name: "q".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: true,
            default_json: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            DependencyStep::ExtractQuery {
                required: true,
                default_json: None,
                ..
            }
        ));
    }

    #[test]
    fn dependency_step_serde_extract_header() {
        let step = DependencyStep::ExtractHeader {
            name: "x_token".to_owned(),
            alias: "x-token".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: true,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractHeader { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_cookie() {
        let step = DependencyStep::ExtractCookie {
            name: "session_id".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: false,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractCookie { .. }));
    }

    #[test]
    fn dependency_step_serde_validate_body() {
        let step = DependencyStep::ValidateBody {
            name: "item".to_owned(),
            model_qualname: QualName::new("backend.models.Item").unwrap(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ValidateBody { .. }));
    }

    // ── DepScope / DepTier serde ─────────────────────────────────────────

    #[test]
    fn dep_scope_serde_roundtrip() {
        for scope in [DepScope::Worker, DepScope::Request] {
            let json = serde_json::to_string(&scope).unwrap();
            let back: DepScope = serde_json::from_str(&json).unwrap();
            assert_eq!(scope, back);
        }
    }

    #[test]
    fn dep_tier_serde_roundtrip() {
        for tier in [DepTier::Native, DepTier::Lifecycle, DepTier::Standard] {
            let json = serde_json::to_string(&tier).unwrap();
            let back: DepTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, back);
        }
    }

    // ── DependencyNode serde ─────────────────────────────────────────────

    #[test]
    fn dependency_node_serde_roundtrip() {
        let node = DependencyNode {
            qualname: QualName::new("backend.deps.get_db").unwrap(),
            tier: DepTier::Standard,
            scope: DepScope::Request,
            is_generator: true,
            is_async: true,
            sub_dependencies: vec![QualName::new("backend.deps.get_session").unwrap()],
            params: vec![ParamManifest {
                name: "conn_str".to_owned(),
                source: ParamSource::Query,
                type_qualname: QualName::new("str").unwrap(),
                required: true,
                json_schema: None,
                alias: None,
                default_json: None,
            }],
        };
        let json = serde_json::to_string(&node).unwrap();
        let back: DependencyNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.qualname.as_str(), "backend.deps.get_db");
        assert_eq!(back.sub_dependencies.len(), 1);
        assert_eq!(back.params.len(), 1);
    }

    // ── LifecycleDepManifest serde ───────────────────────────────────────

    #[test]
    fn lifecycle_dep_manifest_serde_roundtrip() {
        let dep = LifecycleDepManifest {
            qualname: QualName::new("backend.deps.db_engine").unwrap(),
            init_order: 0,
            shutdown_order: 1,
            scope: DepScope::Worker,
        };
        let json = serde_json::to_string(&dep).unwrap();
        let back: LifecycleDepManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.qualname.as_str(), "backend.deps.db_engine");
        assert_eq!(back.init_order, 0);
        assert_eq!(back.shutdown_order, 1);
    }

    // ── ValidationCheck serde ────────────────────────────────────────────

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

    // ── ResponseType Display + serde ─────────────────────────────────────

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

    // ── HttpMethod serde ─────────────────────────────────────────────────

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

    // ── AppModule error variants ─────────────────────────────────────────

    #[test]
    fn app_module_empty_segment() {
        assert!(matches!(
            AppModule::new("foo..bar"),
            Err(AppModuleError::EmptySegment(_))
        ));
    }

    #[test]
    fn app_module_invalid_segment() {
        assert!(matches!(
            AppModule::new("foo.1bar"),
            Err(AppModuleError::InvalidSegment(_))
        ));
    }

    // ── Display / as_str tests ───────────────────────────────────────────

    #[test]
    fn qualname_as_str_and_display() {
        let qn = QualName::new("backend.app.Item").unwrap();
        assert_eq!(qn.as_str(), "backend.app.Item");
        assert_eq!(format!("{qn}"), "backend.app.Item");
    }

    #[test]
    fn app_module_as_str_and_display() {
        let m = AppModule::new("backend.app").unwrap();
        assert_eq!(m.as_str(), "backend.app");
        assert_eq!(format!("{m}"), "backend.app");
    }

    #[test]
    fn route_path_as_str_and_display() {
        let rp = RoutePath::new("/items/{item_id}").unwrap();
        assert_eq!(rp.as_str(), "/items/{item_id}");
        assert_eq!(format!("{rp}"), "/items/{item_id}");
    }

    // ── ProblemTypeUri ───────────────────────────────────────────────────

    #[test]
    fn problem_type_uri_blank_constructor() {
        let uri = ProblemTypeUri::blank();
        assert_eq!(uri.as_str(), "about:blank");
    }
}
