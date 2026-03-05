//! FastAPI app discovery: import module, extract routes, bind to runtime objects.
//!
//! Two phases:
//! 1. **Extract** (import-time): import module → find FastAPI app → extract [`AppManifest`]
//! 2. **Bind** (runtime): resolve manifest entries to live Python objects → [`BoundRoute`]

pub mod bind;
pub mod fastapi;

use crate::route::{AppManifest, AppModule, BoundRoute};
use pyo3::Python;

/// Errors during app discovery.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// Python import or attribute access failed.
    #[error("python error: {0}")]
    Python(String),
    /// The module does not contain a FastAPI instance.
    #[error("no FastAPI instance found in module '{0}'")]
    NoApp(String),
    /// Route manifest extraction failed.
    #[error("invalid route: {0}")]
    InvalidRoute(String),
}

/// Discover routes from a FastAPI app and bind them in one step.
///
/// Imports the Python module, finds the `FastAPI` instance, extracts route
/// metadata, and resolves Python types for runtime dispatch.
///
/// # Errors
///
/// Returns an error if the module cannot be imported, no `FastAPI` app is found,
/// or route metadata is malformed.
pub fn discover_and_bind(
    py: Python<'_>,
    app_module: &AppModule,
) -> Result<(Vec<BoundRoute>, AppManifest), DiscoveryError> {
    let (app, manifest) = fastapi::import_and_extract(py, app_module)?;
    let routes = bind::bind_routes(py, &manifest, &app)?;
    Ok((routes, manifest))
}

// ── Shared helpers ──────────────────────────────────────────────────────

/// Parse an HTTP method string (e.g. `"GET"`) to [`HttpMethod`].
pub fn parse_http_method(s: &str) -> Result<crate::route::HttpMethod, DiscoveryError> {
    use crate::route::HttpMethod;
    match s {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "DELETE" => Ok(HttpMethod::Delete),
        "PATCH" => Ok(HttpMethod::Patch),
        other => Err(DiscoveryError::InvalidRoute(format!(
            "unknown HTTP method: {other}"
        ))),
    }
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

    // ── parse_http_method ────────────────────────────────────────────────

    #[test]
    fn parse_http_method_all_valid() {
        use crate::route::HttpMethod;
        assert_eq!(parse_http_method("GET").unwrap(), HttpMethod::Get);
        assert_eq!(parse_http_method("POST").unwrap(), HttpMethod::Post);
        assert_eq!(parse_http_method("PUT").unwrap(), HttpMethod::Put);
        assert_eq!(parse_http_method("DELETE").unwrap(), HttpMethod::Delete);
        assert_eq!(parse_http_method("PATCH").unwrap(), HttpMethod::Patch);
    }

    #[test]
    fn parse_http_method_unknown() {
        let err = parse_http_method("OPTIONS").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
    }

    // ── HttpMethod::as_str roundtrip ────────────────────────────────────

    #[test]
    fn http_method_as_str_roundtrip() {
        use crate::route::HttpMethod;
        for m in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Delete,
            HttpMethod::Patch,
        ] {
            assert_eq!(parse_http_method(m.as_str()).unwrap(), m);
        }
    }

    // ── DiscoveryError Display ───────────────────────────────────────────

    #[test]
    fn discovery_error_display_python() {
        let err = DiscoveryError::Python("import failed".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("import failed"));
    }

    #[test]
    fn discovery_error_display_no_app() {
        let err = DiscoveryError::NoApp("backend.app".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("backend.app"));
    }

    #[test]
    fn discovery_error_display_invalid_route() {
        let err = DiscoveryError::InvalidRoute("bad path".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("bad path"));
    }
}
