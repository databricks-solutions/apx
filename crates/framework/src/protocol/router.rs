//! Route matching powered by [`matchit`].
//!
//! Pure data transformation — no I/O, no async, no Python callbacks.
//! Takes a path string + method, returns a match result.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Fast path-based router backed by [`matchit::Router`].
///
/// Route patterns use matchit syntax (`{param}`, `{*catch_all}`).
/// Python-side code converts framework-specific syntax before insertion.
#[pyclass(module = "apx._core")]
#[derive(Debug)]
pub struct RustRouter {
    inner: matchit::Router<u32>,
}

/// Error inserting a route pattern.
#[derive(Debug, thiserror::Error)]
#[error("route insert error: {0}")]
struct InsertError(#[from] matchit::InsertError);

#[pymethods]
impl RustRouter {
    /// Create an empty router.
    #[new]
    fn new() -> Self {
        Self {
            inner: matchit::Router::new(),
        }
    }

    /// Insert a route pattern with an opaque integer identifier.
    ///
    /// # Errors
    ///
    /// Returns `ValueError` if the pattern is invalid or conflicts with
    /// an existing route.
    fn insert(&mut self, pattern: &str, route_id: u32) -> PyResult<()> {
        self.inner
            .insert(pattern, route_id)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(InsertError(e).to_string()))
    }

    /// Match a request path against registered routes.
    ///
    /// Returns `(route_id, params_dict)` on match, or `None` if no route
    /// matches the path.
    fn match_route<'py>(
        &self,
        py: Python<'py>,
        path: &str,
    ) -> PyResult<Option<(u32, Bound<'py, PyDict>)>> {
        let Ok(matched) = self.inner.at(path) else {
            return Ok(None);
        };

        let route_id = *matched.value;
        let params = PyDict::new(py);
        for (key, value) in matched.params.iter() {
            params.set_item(key, value)?;
        }

        Ok(Some((route_id, params)))
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_basic_match() {
        let mut router = RustRouter::new();
        router.insert("/users/{id}", 1).ok();
        router.insert("/health", 2).ok();

        crate::with_py(|py| {
            let result = router.match_route(py, "/users/42").ok().flatten();
            assert!(result.is_some());
            let (route_id, params) = result.as_ref().map(|(id, p)| (*id, p)).unwrap();
            assert_eq!(route_id, 1);
            let id_val: String = params.get_item("id").unwrap().unwrap().extract().unwrap();
            assert_eq!(id_val, "42");

            let result = router.match_route(py, "/health").ok().flatten();
            assert!(result.is_some());
            let (route_id, params) = result.as_ref().map(|(id, p)| (*id, p)).unwrap();
            assert_eq!(route_id, 2);
            assert!(params.is_empty());
        });
    }

    #[test]
    fn test_no_match() {
        let router = RustRouter::new();

        crate::with_py(|py| {
            let result = router.match_route(py, "/nonexistent").ok().flatten();
            assert!(result.is_none());
        });
    }

    #[test]
    fn test_catch_all() {
        let mut router = RustRouter::new();
        router.insert("/static/{*filepath}", 1).ok();

        crate::with_py(|py| {
            let result = router
                .match_route(py, "/static/css/style.css")
                .ok()
                .flatten();
            assert!(result.is_some());
            let (route_id, params) = result.as_ref().map(|(id, p)| (*id, p)).unwrap();
            assert_eq!(route_id, 1);
            let fp: String = params
                .get_item("filepath")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(fp, "css/style.css");
        });
    }

    #[test]
    fn test_multiple_params() {
        let mut router = RustRouter::new();
        router.insert("/orgs/{org}/repos/{repo}", 1).ok();

        crate::with_py(|py| {
            let result = router
                .match_route(py, "/orgs/acme/repos/widgets")
                .ok()
                .flatten();
            assert!(result.is_some());
            let (_, params) = result.as_ref().map(|(id, p)| (*id, p)).unwrap();
            let org: String = params.get_item("org").unwrap().unwrap().extract().unwrap();
            let repo: String = params.get_item("repo").unwrap().unwrap().extract().unwrap();
            assert_eq!(org, "acme");
            assert_eq!(repo, "widgets");
        });
    }

    #[test]
    fn test_insert_conflict() {
        let mut router = RustRouter::new();
        router.insert("/users/{id}", 1).ok();
        let result = router.insert("/users/{name}", 2);
        assert!(result.is_err());
    }
}
