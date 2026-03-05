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

/// Parse a param source string to [`ParamSource`].
#[allow(
    dead_code,
    reason = "tested utility — will be used by manifest loading path"
)]
pub fn parse_param_source(source: &str) -> Result<crate::route::ParamSource, DiscoveryError> {
    use crate::route::ParamSource;
    match source {
        "path" => Ok(ParamSource::Path),
        "query" => Ok(ParamSource::Query),
        "header" => Ok(ParamSource::Header),
        "cookie" => Ok(ParamSource::Cookie),
        "body" => Ok(ParamSource::Body),
        "raw_body" => Ok(ParamSource::RawBody),
        "raw_request" => Ok(ParamSource::RawRequest),
        other => Err(DiscoveryError::InvalidRoute(format!(
            "unknown param source: {other}"
        ))),
    }
}

/// Parse a response type string from Python.
#[allow(
    dead_code,
    reason = "tested utility — will be used by manifest loading path"
)]
pub fn parse_response_type(s: &str) -> Result<crate::route::ResponseType, DiscoveryError> {
    use crate::route::{QualName, ResponseType};
    if let Some(qualname_str) = s.strip_prefix("model:") {
        let qualname = QualName::new(qualname_str).map_err(|e| {
            DiscoveryError::InvalidRoute(format!("response model '{qualname_str}': {e}"))
        })?;
        Ok(ResponseType::Model {
            qualname,
            json_schema: None,
            status_code: 200,
        })
    } else {
        Ok(ResponseType::RawResponse)
    }
}

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

/// Return the uppercase string for an [`HttpMethod`].
pub fn http_method_str(m: crate::route::HttpMethod) -> &'static str {
    use crate::route::HttpMethod;
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Patch => "PATCH",
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
    use crate::route::{ParamSource, ResponseType};

    // ── parse_param_source ───────────────────────────────────────────────

    #[test]
    fn parse_param_source_path() {
        assert_eq!(parse_param_source("path").unwrap(), ParamSource::Path);
    }

    #[test]
    fn parse_param_source_query() {
        assert_eq!(parse_param_source("query").unwrap(), ParamSource::Query);
    }

    #[test]
    fn parse_param_source_header() {
        assert_eq!(parse_param_source("header").unwrap(), ParamSource::Header);
    }

    #[test]
    fn parse_param_source_cookie() {
        assert_eq!(parse_param_source("cookie").unwrap(), ParamSource::Cookie);
    }

    #[test]
    fn parse_param_source_body() {
        assert_eq!(parse_param_source("body").unwrap(), ParamSource::Body);
    }

    #[test]
    fn parse_param_source_raw_body() {
        assert_eq!(
            parse_param_source("raw_body").unwrap(),
            ParamSource::RawBody
        );
    }

    #[test]
    fn parse_param_source_raw_request() {
        assert_eq!(
            parse_param_source("raw_request").unwrap(),
            ParamSource::RawRequest
        );
    }

    #[test]
    fn parse_param_source_unknown() {
        let err = parse_param_source("unknown").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
        let msg = format!("{err}");
        assert!(msg.contains("unknown"));
    }

    // ── parse_response_type ──────────────────────────────────────────────

    #[test]
    fn parse_response_type_model_valid() {
        let rt = parse_response_type("model:backend.models.Item").unwrap();
        match rt {
            ResponseType::Model {
                qualname,
                json_schema,
                status_code,
            } => {
                assert_eq!(qualname.as_str(), "backend.models.Item");
                assert!(json_schema.is_none());
                assert_eq!(status_code, 200);
            }
            _ => panic!("expected Model"),
        }
    }

    #[test]
    fn parse_response_type_model_invalid_qualname() {
        let err = parse_response_type("model:").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRoute(_)));
    }

    #[test]
    fn parse_response_type_raw_fallback() {
        let rt = parse_response_type("raw_response").unwrap();
        assert!(matches!(rt, ResponseType::RawResponse));
    }

    #[test]
    fn parse_response_type_anything_else_is_raw() {
        let rt = parse_response_type("something_else").unwrap();
        assert!(matches!(rt, ResponseType::RawResponse));
    }

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

    // ── http_method_str ──────────────────────────────────────────────────

    #[test]
    fn http_method_str_roundtrip() {
        use crate::route::HttpMethod;
        for m in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Delete,
            HttpMethod::Patch,
        ] {
            let s = http_method_str(m);
            assert_eq!(parse_http_method(s).unwrap(), m);
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
