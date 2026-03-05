//! Per-worker lifecycle dependency cache.
//!
//! Stores values resolved once per worker lifetime (e.g. DB engine, config).
//! Full lifecycle initialization (async generators, shutdown cleanup) is
//! deferred to Phase 6.

use pyo3::{Py, PyAny};
use std::collections::HashMap;

/// Cache of lifecycle-scoped dependency values, shared across all routes.
///
/// Populated during worker startup, read during request dispatch by
/// [`PlanExecutorDispatch`](crate::bridge::plan_executor::PlanExecutorDispatch).
#[derive(Debug)]
pub struct LifecycleCache {
    /// Internal storage. `pub(crate)` for test access only.
    pub(crate) values: HashMap<String, Py<PyAny>>,
}

impl LifecycleCache {
    /// Create an empty cache (no lifecycle deps resolved yet).
    pub fn empty() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    /// Look up a cached lifecycle dependency by qualified name.
    pub fn get(&self, qualname: &str) -> Option<&Py<PyAny>> {
        self.values.get(qualname)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_cache_empty_returns_none() {
        let cache = LifecycleCache::empty();
        assert!(cache.get("some.dep").is_none());
    }

    #[test]
    fn lifecycle_cache_get_stored_value() {
        pyo3::Python::initialize();
        let mut cache = LifecycleCache::empty();
        pyo3::Python::attach(|py| {
            cache.values.insert("my.dep".to_owned(), py.None());
        });
        assert!(cache.get("my.dep").is_some());
    }

    #[test]
    fn lifecycle_cache_get_missing_key() {
        pyo3::Python::initialize();
        let mut cache = LifecycleCache::empty();
        pyo3::Python::attach(|py| {
            cache.values.insert("other.dep".to_owned(), py.None());
        });
        assert!(cache.get("missing.dep").is_none());
    }
}
