//! Runtime-bound types that carry live Python objects.
//!
//! Constructed during route discovery, consumed by the bridge layer.
//! These types are `!Clone` because `Py<PyAny>` requires the GIL for cloning.

use super::manifest::RouteManifest;
use pyo3::types::PyAny;
use pyo3::{Py, Python};
use std::fmt;

// ── Handler ─────────────────────────────────────────────────────────────

/// A Python callable that handles an HTTP request.
///
/// Wraps the endpoint function discovered from a route.
/// Methods centralize all call-site Python interop.
pub struct Handler(Py<PyAny>);

impl Handler {
    /// Wrap a Python callable, asserting it's actually callable.
    pub fn new(py: Python<'_>, obj: Py<PyAny>) -> Self {
        use pyo3::types::PyAnyMethods;
        debug_assert!(
            obj.bind(py).is_callable(),
            "Handler: object is not callable"
        );
        Self(obj)
    }

    /// Create a stub handler for unit tests (skips callable check).
    #[cfg(test)]
    pub fn stub(obj: Py<PyAny>) -> Self {
        Self(obj)
    }

    /// Borrow the inner reference (for ASGI bridge dispatch).
    pub fn inner(&self) -> &Py<PyAny> {
        &self.0
    }
}

impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Handler").field(&"<callable>").finish()
    }
}

// ── App ─────────────────────────────────────────────────────────────────

/// The live ASGI application instance (for dependency_overrides, middleware).
pub struct App(Py<PyAny>);

impl App {
    pub fn new(obj: Py<PyAny>) -> Self {
        Self(obj)
    }

    /// Clone the reference (requires GIL).
    pub fn clone_ref(&self, py: Python<'_>) -> Self {
        Self(self.0.clone_ref(py))
    }

    /// Borrow the inner reference.
    pub fn inner(&self) -> &Py<PyAny> {
        &self.0
    }
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("App").field(&"<app>").finish()
    }
}

// ── BoundRoute ──────────────────────────────────────────────────────────

/// A route bound to its runtime implementation.
///
/// Shared via `Arc<BoundRoute>` — never cloned (`Py<PyAny>` fields are not
/// `Clone`-safe without the GIL).
///
/// Constructed in [`discovery`](crate::discovery), consumed in [`bridge`](crate::bridge).
pub struct BoundRoute {
    /// Serializable route metadata.
    pub manifest: RouteManifest,
    /// Python handler callable (for WS) or the ASGI app callable (for HTTP via FastAPI).
    pub handler: Handler,
    /// Reference to the live FastAPI app (for ASGI bridge dispatch).
    pub fastapi_app: Option<App>,
}

impl fmt::Debug for BoundRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundRoute")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}
