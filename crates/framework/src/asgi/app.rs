//! App loader: import a Python ASGI callable at runtime.
//!
//! Parses specifiers like `"myapp.main:app"` or `"myapp"` (attr defaults to
//! `"app"`) and imports the callable via `importlib.import_module`.
//!
//! The [`AppSource`] trait is the extension seam for app loading strategies.
//! `ModuleImport` is the live-import implementation; a future `ManifestSource`
//! will provide pre-built dispatch pipelines.

use crate::asgi::dispatch::AsgiDispatch;
use crate::asgi::queue::RequestQueue;
use crate::asgi::scope::ScopeInterns;
use crate::dispatch::Dispatch;
use crate::supervision::worker_context::WorkerContext;
use pyo3::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;

// ── Constants ────────────────────────────────────────────────────────────

/// Separator between module path and attribute name in a specifier.
const SPECIFIER_SEPARATOR: char = ':';

/// Default attribute name when no separator is present.
const DEFAULT_ATTR: &str = "app";

// ── AsgiApp ──────────────────────────────────────────────────────────────

/// A Python ASGI callable, held as a GIL-independent reference.
#[derive(Debug)]
pub struct AsgiApp(Py<PyAny>);

impl AsgiApp {
    /// Access the inner Python object.
    pub fn inner(&self) -> &Py<PyAny> {
        &self.0
    }
}

// ── AppLoadError ─────────────────────────────────────────────────────────

/// Errors that can occur when loading a Python ASGI app.
#[derive(Debug, thiserror::Error)]
pub enum AppLoadError {
    /// The specifier string is malformed (empty, empty module, or empty attr).
    #[error("invalid specifier {specifier:?}: expected \"module:attr\" or \"module\"")]
    InvalidSpecifier {
        /// The malformed specifier.
        specifier: String,
    },

    /// `importlib.import_module` failed.
    #[error("failed to import module {module:?}: {source}")]
    ImportFailed {
        /// Module path that could not be imported.
        module: String,
        /// Underlying Python exception.
        source: PyErr,
    },

    /// The module was imported but has no attribute with the given name.
    #[error("module {module:?} has no attribute {attr:?}")]
    MissingAttribute {
        /// Module path.
        module: String,
        /// Attribute name that was not found.
        attr: String,
    },

    /// The attribute exists but is not callable.
    #[error("{module}:{attr} is not callable")]
    NotCallable {
        /// Module path.
        module: String,
        /// Attribute name.
        attr: String,
    },
}

// ── format_pyerr ─────────────────────────────────────────────────────────

/// Render a Python exception with its full traceback.
///
/// Uses `traceback.format_exception(err)` to produce the same multi-line
/// output that Python prints on unhandled exceptions. Falls back to
/// `PyErr`'s `Display` (type + message only) if the traceback module is
/// unavailable or the formatting call itself fails.
pub fn format_pyerr(py: Python<'_>, err: &PyErr) -> String {
    format_pyerr_inner(py, err).unwrap_or_else(|| err.to_string())
}

/// Inner helper: returns `None` on any failure so the caller can fall back.
fn format_pyerr_inner(py: Python<'_>, err: &PyErr) -> Option<String> {
    let tb_mod = py.import(c"traceback").ok()?;
    let lines = tb_mod
        .call_method1(c"format_exception", (err.value(py),))
        .ok()?;
    let joined: String = lines.extract::<Vec<String>>().ok()?.join("");
    Some(joined)
}

// ── parse_specifier ──────────────────────────────────────────────────────

/// Split a specifier into `(module, attr)`.
///
/// Supports `"module:attr"` (explicit) and `"module"` (attr defaults to `"app"`).
fn parse_specifier(specifier: &str) -> Result<(&str, &str), AppLoadError> {
    if specifier.is_empty() {
        return Err(AppLoadError::InvalidSpecifier {
            specifier: specifier.to_owned(),
        });
    }

    if let Some((module, attr)) = specifier.split_once(SPECIFIER_SEPARATOR) {
        if module.is_empty() || attr.is_empty() {
            return Err(AppLoadError::InvalidSpecifier {
                specifier: specifier.to_owned(),
            });
        }
        Ok((module, attr))
    } else {
        Ok((specifier, DEFAULT_ATTR))
    }
}

// ── AppSource ────────────────────────────────────────────────────────────

/// Maximum request body size: 10 MiB.
const DEFAULT_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// Load an ASGI application and build its dispatch pipeline.
///
/// Implementations decide how the app is located (runtime import, manifest,
/// etc.) and which dispatch strategy to use. The returned `Arc<dyn Dispatch>`
/// is handed to `ApxService` and shared across all connections.
pub trait AppSource: Send + Sync + std::fmt::Debug {
    /// Load the app and construct its dispatch pipeline.
    ///
    /// Called once per worker with the GIL held. `event_loop_py` is the
    /// asyncio event loop object needed by `install_dispatch`.
    ///
    /// # Errors
    ///
    /// Returns [`AppLoadError`] if the app cannot be loaded.
    fn build(
        &self,
        py: Python<'_>,
        ctx: Arc<WorkerContext>,
        event_loop_py: &Py<PyAny>,
        server_addr: SocketAddr,
    ) -> Result<Arc<dyn Dispatch>, AppLoadError>;
}

// ── ModuleImport ─────────────────────────────────────────────────────────

/// Runtime import of a Python ASGI callable from a `"module:attr"` specifier.
#[derive(Debug)]
pub struct ModuleImport {
    specifier: String,
    dev_mode: bool,
}

impl ModuleImport {
    /// Create a new loader from a specifier string.
    pub fn new(specifier: impl Into<String>, dev_mode: bool) -> Self {
        Self {
            specifier: specifier.into(),
            dev_mode,
        }
    }

    /// Import the module and resolve the callable attribute.
    ///
    /// Must be called while holding the GIL.
    ///
    /// # Errors
    ///
    /// Returns [`AppLoadError`] if the specifier is invalid, the module cannot
    /// be imported, the attribute is missing, or the attribute is not callable.
    pub fn load_callable(&self, py: Python<'_>) -> Result<AsgiApp, AppLoadError> {
        let (module_path, attr_name) = parse_specifier(&self.specifier)?;

        let importlib = py
            .import(c"importlib")
            .map_err(|e| AppLoadError::ImportFailed {
                module: "importlib".to_owned(),
                source: e,
            })?;

        let module = importlib
            .call_method1(c"import_module", (module_path,))
            .map_err(|e| AppLoadError::ImportFailed {
                module: module_path.to_owned(),
                source: e,
            })?;

        let attr = module
            .getattr(attr_name)
            .map_err(|_| AppLoadError::MissingAttribute {
                module: module_path.to_owned(),
                attr: attr_name.to_owned(),
            })?;

        if !attr.is_callable() {
            return Err(AppLoadError::NotCallable {
                module: module_path.to_owned(),
                attr: attr_name.to_owned(),
            });
        }

        Ok(AsgiApp(attr.unbind()))
    }
}

impl AppSource for ModuleImport {
    fn build(
        &self,
        py: Python<'_>,
        ctx: Arc<WorkerContext>,
        event_loop_py: &Py<PyAny>,
        server_addr: SocketAddr,
    ) -> Result<Arc<dyn Dispatch>, AppLoadError> {
        let app = self.load_callable(py)?;
        let interns = Arc::new(ScopeInterns::new(py, server_addr));

        let queue = RequestQueue::new(
            py,
            &ctx.pipeline.inbound,
            Arc::clone(&ctx.pipeline.wakeup),
            Arc::clone(&interns),
            self.dev_mode,
        )
        .map_err(|e| AppLoadError::ImportFailed {
            module: "RequestQueue".to_owned(),
            source: e,
        })?;
        let queue_obj = Py::new(py, queue).map_err(|e| AppLoadError::ImportFailed {
            module: "RequestQueue".to_owned(),
            source: e,
        })?;

        let wakeup_fd = ctx.pipeline.wakeup.reader_fd();
        let dispatch_mod = py
            .import(c"apx._dispatch")
            .map_err(|e| AppLoadError::ImportFailed {
                module: "apx._dispatch".to_owned(),
                source: e,
            })?;
        dispatch_mod
            .call_method1(
                c"install_dispatch",
                (event_loop_py, queue_obj, app.inner(), wakeup_fd),
            )
            .map_err(|e| AppLoadError::ImportFailed {
                module: "install_dispatch".to_owned(),
                source: e,
            })?;

        let dispatch = AsgiDispatch::new(
            ctx.pipeline.inbound.sender().clone(),
            Arc::clone(&ctx.pipeline.wakeup),
            DEFAULT_BODY_LIMIT,
            app.inner().clone_ref(py),
            interns,
            ctx,
        );
        Ok(Arc::new(dispatch))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    // ── parse_specifier unit tests ───────────────────────────────────────

    #[test]
    fn parse_valid_specifier() {
        let (module, attr) = parse_specifier("myapp.main:app").unwrap();
        assert_eq!(module, "myapp.main");
        assert_eq!(attr, "app");
    }

    #[test]
    fn parse_specifier_plain_module() {
        let (module, attr) = parse_specifier("myapp.main").unwrap();
        assert_eq!(module, "myapp.main");
        assert_eq!(attr, "app");
    }

    #[test]
    fn parse_specifier_single_segment() {
        let (module, attr) = parse_specifier("app").unwrap();
        assert_eq!(module, "app");
        assert_eq!(attr, "app");
    }

    #[test]
    fn parse_specifier_empty_string() {
        assert!(matches!(
            parse_specifier(""),
            Err(AppLoadError::InvalidSpecifier { .. })
        ));
    }

    #[test]
    fn parse_specifier_empty_module() {
        assert!(matches!(
            parse_specifier(":app"),
            Err(AppLoadError::InvalidSpecifier { .. })
        ));
    }

    #[test]
    fn parse_specifier_empty_attr() {
        assert!(matches!(
            parse_specifier("myapp:"),
            Err(AppLoadError::InvalidSpecifier { .. })
        ));
    }

    // ── Python integration tests ─────────────────────────────────────────

    #[test]
    fn load_builtin_callable() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("json:dumps", false);
            let app = loader.load_callable(py).unwrap();
            assert!(app.inner().bind(py).is_callable());
        });
    }

    #[test]
    fn load_plain_module_default_attr_fails() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("json", false);
            let err = loader.load_callable(py).unwrap_err();
            assert!(matches!(err, AppLoadError::MissingAttribute { .. }));
        });
    }

    #[test]
    fn load_missing_module() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("nonexistent_module_xyz:app", false);
            let err = loader.load_callable(py).unwrap_err();
            assert!(matches!(err, AppLoadError::ImportFailed { .. }));
        });
    }

    #[test]
    fn load_missing_attr() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("json:nonexistent_attr_xyz", false);
            let err = loader.load_callable(py).unwrap_err();
            assert!(matches!(err, AppLoadError::MissingAttribute { .. }));
        });
    }

    #[test]
    fn load_not_callable() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("json:__name__", false);
            let err = loader.load_callable(py).unwrap_err();
            assert!(matches!(err, AppLoadError::NotCallable { .. }));
        });
    }

    // ── Error display tests ──────────────────────────────────────────────

    #[test]
    fn error_display_invalid_specifier() {
        let err = AppLoadError::InvalidSpecifier {
            specifier: "bad".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("module:attr"));
    }

    #[test]
    fn error_display_import_failed() {
        crate::with_py(|py| {
            let loader = ModuleImport::new("nonexistent_module_xyz:app", false);
            let err = loader.load_callable(py).unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("import"));
        });
    }

    #[test]
    fn error_display_missing_attribute() {
        let err = AppLoadError::MissingAttribute {
            module: "json".to_owned(),
            attr: "nope".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("no attribute"));
    }

    #[test]
    fn error_display_not_callable() {
        let err = AppLoadError::NotCallable {
            module: "json".to_owned(),
            attr: "__name__".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("not callable"));
    }

    // ── format_pyerr tests ───────────────────────────────────────────────

    #[test]
    fn format_pyerr_includes_traceback_lines() {
        crate::with_py(|py| {
            // Execute code that raises an exception with a traceback.
            let result = py.run(c"exec('raise ValueError(\"test error\")')", None, None);
            let err = result.unwrap_err();
            let formatted = format_pyerr(py, &err);

            assert!(
                formatted.contains("ValueError"),
                "should contain exception type, got: {formatted}"
            );
            assert!(
                formatted.contains("test error"),
                "should contain exception message, got: {formatted}"
            );
        });
    }

    #[test]
    fn format_pyerr_fallback_on_simple_err() {
        crate::with_py(|py| {
            let err = pyo3::exceptions::PyRuntimeError::new_err("simple error");
            let formatted = format_pyerr(py, &err);
            assert!(
                formatted.contains("RuntimeError") || formatted.contains("simple error"),
                "should contain error info, got: {formatted}"
            );
        });
    }
}
