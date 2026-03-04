//! Request context extracted from HTTP before entering Python.
//!
//! Contains all data needed to build Python handler kwargs.

use bytes::Bytes;
use http::HeaderMap;

/// Everything extracted from the HTTP request before entering Python.
///
/// Fields are `pub` — only the bridge and param modules need access.
pub struct RequestContext {
    /// Path parameters extracted by axum: `[("item_id", "42")]`.
    /// Typically 0–3 entries; `Vec` is faster than `HashMap`.
    pub path_params: Vec<(String, String)>,
    /// Query parameters: `[("page", "1"), ("sort", "name")]`.
    pub query_params: Vec<(String, String)>,
    /// HTTP headers.
    pub headers: HeaderMap,
    /// Request body bytes. `None` when the route has no Body param
    /// (skip reading for GET-style routes).
    pub body: Option<Bytes>,
}

impl std::fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContext")
            .field("path_params", &self.path_params)
            .field("query_params", &self.query_params)
            .field("body_len", &self.body.as_ref().map(Bytes::len))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn request_context_debug_with_body() {
        let ctx = RequestContext {
            path_params: vec![("id".to_owned(), "42".to_owned())],
            query_params: vec![("page".to_owned(), "1".to_owned())],
            headers: HeaderMap::new(),
            body: Some(Bytes::from("hello")),
        };
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("body_len"));
        assert!(dbg.contains('5'));
    }

    #[test]
    fn request_context_debug_without_body() {
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
        };
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("None"));
    }
}
