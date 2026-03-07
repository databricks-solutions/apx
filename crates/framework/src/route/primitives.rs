//! Validated domain primitives: Python paths, route paths, body limits, and RFC 9457 URIs.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

// ── Dotted path validation ───────────────────────────────────────────────

/// Validate a single segment of a Python dotted path.
///
/// A valid segment is a Python identifier: non-empty, does not start with
/// an ASCII digit, and contains only alphanumeric characters or underscores.
fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with(|c: char| c.is_ascii_digit())
        && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Validation errors for Python dotted paths ([`QualName`], [`AppModule`]).
#[derive(Debug, thiserror::Error)]
pub enum DottedPathError {
    /// The path was empty.
    #[error("{context} must not be empty")]
    Empty {
        /// What kind of path failed validation.
        context: &'static str,
    },
    /// A segment between dots was empty (e.g. `"foo..bar"`).
    #[error("{context} has empty segment: {value}")]
    EmptySegment {
        /// What kind of path failed validation.
        context: &'static str,
        /// The original input.
        value: String,
    },
    /// A segment is not a valid Python identifier.
    #[error("invalid {context} segment: {segment}")]
    InvalidSegment {
        /// What kind of path failed validation.
        context: &'static str,
        /// The invalid segment.
        segment: String,
    },
}

/// Validate a Python dotted path (e.g. `"backend.app.handler"`).
///
/// `context` is used in error messages (e.g. `"qualified name"`, `"app module"`).
fn validate_dotted_path(path: &str, context: &'static str) -> Result<(), DottedPathError> {
    if path.is_empty() {
        return Err(DottedPathError::Empty { context });
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(DottedPathError::EmptySegment {
                context,
                value: path.to_owned(),
            });
        }
        if !is_valid_segment(segment) {
            return Err(DottedPathError::InvalidSegment {
                context,
                segment: segment.to_owned(),
            });
        }
    }
    Ok(())
}

// ── QualName ────────────────────────────────────────────────────────────

/// Python qualified name: `"backend.app.create_item"`, `"int"`, `"str"`.
///
/// Used for handler references, parameter type identification, and OpenAPI
/// schema generation. Each dot-separated segment must be a valid Python
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualName(String);

impl QualName {
    /// Error context for validation messages.
    const CONTEXT: &str = "qualified name";

    /// Create a new qualified name, validating all segments.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty or any segment is invalid.
    pub fn new(name: impl Into<String>) -> Result<Self, DottedPathError> {
        let name = name.into();
        validate_dotted_path(&name, Self::CONTEXT)?;
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

// ── AppModule ───────────────────────────────────────────────────────────

/// Python module path: `"backend.app"`, `"mypackage.api"`.
///
/// Must be a valid Python dotted path. Format validation only — runtime
/// validation (module importable, contains App instance) happens during
/// worker discovery.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppModule(String);

impl AppModule {
    /// Error context for validation messages.
    const CONTEXT: &str = "app module";

    /// Create a new module path, validating all segments.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is empty or any segment is invalid.
    pub fn new(module: impl Into<String>) -> Result<Self, DottedPathError> {
        let module = module.into();
        validate_dotted_path(&module, Self::CONTEXT)?;
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

// ── RoutePath ───────────────────────────────────────────────────────────

/// Validated route path template: `"/items/{item_id}"`.
///
/// Must start with `'/'`. FastAPI catch-all convertors (`{param:path}`)
/// are automatically translated to axum/matchit wildcards (`{*param}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoutePath(String);

/// Validation errors for [`RoutePath`].
#[derive(Debug, thiserror::Error)]
pub enum RoutePathError {
    /// Path did not start with `/`.
    #[error("route path must start with '/', got: {0}")]
    MissingLeadingSlash(String),
}

/// A single path segment classification.
enum Segment<'a> {
    /// Verbatim text (e.g. `items`).
    Literal(&'a str),
    /// Named parameter (e.g. `{item_id}` — kept as-is, axum understands it).
    Param(&'a str),
    /// Catch-all parameter (e.g. `{file_path:path}` → `{*file_path}`).
    CatchAll(&'a str),
}

/// Classify a single `/`-delimited path segment.
fn classify_segment(segment: &str) -> Segment<'_> {
    let Some(inner) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return Segment::Literal(segment);
    };
    match inner.strip_suffix(":path") {
        Some(name) => Segment::CatchAll(name),
        None => Segment::Param(segment),
    }
}

/// Render a classified segment back to axum/matchit syntax.
fn render_segment(segment: Segment<'_>) -> Cow<'_, str> {
    match segment {
        Segment::Literal(s) | Segment::Param(s) => Cow::Borrowed(s),
        Segment::CatchAll(name) => Cow::Owned(format!("{{*{name}}}")),
    }
}

/// Convert FastAPI path syntax to axum/matchit syntax.
///
/// Translates `{param:path}` catch-all convertors to `{*param}` wildcards.
/// Other segments pass through unchanged.
fn to_axum_syntax(path: &str) -> Cow<'_, str> {
    if !path.contains(":path}") {
        return Cow::Borrowed(path);
    }
    let converted: String = path
        .split('/')
        .map(|seg| render_segment(classify_segment(seg)))
        .collect::<Vec<_>>()
        .join("/");
    Cow::Owned(converted)
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

    /// Return the inner string (FastAPI syntax).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the path in axum/matchit syntax.
    ///
    /// Translates FastAPI `{param:path}` catch-all convertors to `{*param}`.
    /// Returns a borrowed reference if no conversion is needed.
    pub fn as_axum_str(&self) -> Cow<'_, str> {
        to_axum_syntax(&self.0)
    }
}

impl fmt::Display for RoutePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ── BodyLimit ───────────────────────────────────────────────────────────

/// Max request body size in bytes.
///
/// Newtype prevents mixing with other `usize` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyLimit(pub usize);

impl BodyLimit {
    /// Default body limit: 1 MiB.
    pub const DEFAULT: Self = Self(1024 * 1024);
}

// ── ProblemTypeUri ──────────────────────────────────────────────────────

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

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_segment_valid() {
        assert!(is_valid_segment("foo_bar"));
        assert!(is_valid_segment("_private"));
        assert!(is_valid_segment("CamelCase"));
    }

    #[test]
    fn is_valid_segment_empty() {
        assert!(!is_valid_segment(""));
    }

    #[test]
    fn is_valid_segment_leading_digit() {
        assert!(!is_valid_segment("1abc"));
    }

    #[test]
    fn is_valid_segment_hyphen() {
        assert!(!is_valid_segment("foo-bar"));
    }

    #[test]
    fn is_valid_segment_unicode() {
        assert!(is_valid_segment("café"));
    }

    #[test]
    fn qualname_valid() {
        assert!(QualName::new("int").is_ok());
        assert!(QualName::new("backend.app.Item").is_ok());
    }

    #[test]
    fn qualname_empty() {
        assert!(matches!(
            QualName::new(""),
            Err(DottedPathError::Empty { .. })
        ));
    }

    #[test]
    fn qualname_empty_segment() {
        assert!(matches!(
            QualName::new("foo..bar"),
            Err(DottedPathError::EmptySegment { .. })
        ));
    }

    #[test]
    fn qualname_invalid_segment() {
        assert!(matches!(
            QualName::new("foo.1bar"),
            Err(DottedPathError::InvalidSegment { .. })
        ));
    }

    #[test]
    fn app_module_valid() {
        assert!(AppModule::new("backend.app").is_ok());
        assert!(AppModule::new("app").is_ok());
    }

    #[test]
    fn app_module_empty() {
        assert!(matches!(
            AppModule::new(""),
            Err(DottedPathError::Empty { .. })
        ));
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
    fn route_path_axum_converts_catch_all() {
        let rp = RoutePath::new("/files/{file_path:path}").unwrap();
        assert_eq!(rp.as_str(), "/files/{file_path:path}");
        assert_eq!(rp.as_axum_str().as_ref(), "/files/{*file_path}");
    }

    #[test]
    fn route_path_axum_preserves_normal_params() {
        let rp = RoutePath::new("/items/{item_id}").unwrap();
        assert_eq!(rp.as_axum_str().as_ref(), "/items/{item_id}");
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "route path template, not a format string"
    )]
    fn route_path_axum_mixed_params_and_catch_all() {
        let rp = RoutePath::new("/repos/{owner}/{repo}/files/{file_path:path}").unwrap();
        assert_eq!(
            rp.as_axum_str().as_ref(),
            "/repos/{owner}/{repo}/files/{*file_path}"
        );
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

    #[test]
    fn problem_type_uri_blank_constructor() {
        let uri = ProblemTypeUri::blank();
        assert_eq!(uri.as_str(), "about:blank");
    }

    #[test]
    fn app_module_empty_segment() {
        assert!(matches!(
            AppModule::new("foo..bar"),
            Err(DottedPathError::EmptySegment { .. })
        ));
    }

    #[test]
    fn app_module_invalid_segment() {
        assert!(matches!(
            AppModule::new("foo.1bar"),
            Err(DottedPathError::InvalidSegment { .. })
        ));
    }
}
