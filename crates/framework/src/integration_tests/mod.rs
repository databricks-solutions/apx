//! Integration tests: full serve path (discovery → router → TCP → HTTP).
//!
//! Tests are grouped by feature area:
//! - [`handlers`] — basic dispatch, HTTP methods, sync handlers, routing
//! - [`params`] — path, query, header, cookie, enum parameters
//! - [`body`] — JSON body, form data, file upload, mixed parameters
//! - [`responses`] — status codes, response types, streaming, SSE
//! - [`dependencies`] — `Depends()`, generators, background tasks, overrides
//! - [`middleware`] — error handling, middleware, lifespan, request/response injection
//! - [`routing`] — 404/405, health probes, trailing slash, multiple routers

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]

mod body;
mod dependencies;
mod handlers;
mod manifest;
mod middleware;
mod params;
mod responses;
mod routing;
mod scheduler_compat;

use crate::bridge::asgi::lifespan::{LifespanGuard, run_lifespan_startup};
use crate::bridge::dispatch::AppState;
use crate::bridge::{build_router, wrap_layers};
use crate::discovery;
use crate::event_loop::EventLoop;
use crate::route::{AppModule, BodyLimit};
use crate::transport::{Listener, TcpListener, TransportConfig};
use crate::with_py;
use pyo3::types::PyAnyMethods;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, Once, OnceLock};

// ── Discovery lock ──────────────────────────────────────────────────────

/// Serialize sys.path manipulation + module import across parallel tests.
///
/// Python's import machinery reads `sys.path` at import time. Without this
/// lock, concurrent tests inserting different temp dirs into `sys.path` can
/// cause one test's module to be discovered under another test's namespace.
static DISCOVERY_LOCK: Mutex<()> = Mutex::new(());

/// Shared event loop for all integration tests.
///
/// Lazily created on first use. Never stopped — lives for the process
/// lifetime. Reduces GIL contention from N concurrent event loop threads
/// to exactly one.
static SHARED_EVENT_LOOP: Mutex<Option<EventLoop>> = Mutex::new(None);

/// Get a handle to the shared event loop, creating it if needed.
fn shared_event_loop_handle() -> crate::event_loop::EventLoopHandle {
    let mut guard = SHARED_EVENT_LOOP.lock().unwrap_or_else(|e| e.into_inner());
    let event_loop = guard.get_or_insert_with(|| EventLoop::start().unwrap());
    event_loop.handle().unwrap()
}

// ── Tracing setup ───────────────────────────────────────────────────────

static TRACING_INIT: Once = Once::new();

/// Ensure a tracing subscriber is registered so framework traces are visible
/// in test output when `APX_LOG` is set. Safe to call from any test — runs
/// exactly once, silently ignored if a subscriber is already set.
fn ensure_tracing() {
    TRACING_INIT.call_once(|| {
        apx_common::tracing_fmt::init_fmt_subscriber("apx_framework");
    });
}

// ── Python environment setup ────────────────────────────────────────────

static PYTHON_ENV_INIT: Once = Once::new();
static PYTHON_VERSION: OnceLock<String> = OnceLock::new();

/// Ensure `PYTHONHOME` and `VIRTUAL_ENV` are set so the embedded interpreter
/// can find its stdlib. Safe to call from any test — runs exactly once.
///
/// **Testing harness only** — this module is `#[cfg(test)]` and must not be
/// used in production code paths.
#[expect(unsafe_code, reason = "env::set_var required for Python interpreter")]
pub fn ensure_python_env() {
    PYTHON_ENV_INIT.call_once(|| {
        if std::env::var("PYTHONHOME").is_ok() {
            // CI or manual override — still parse version for site-packages.
            parse_python_version();
            return;
        }
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
        let venv = workspace_root.join(".venv");
        let cfg_path = venv.join("pyvenv.cfg");
        let cfg = std::fs::read_to_string(&cfg_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", cfg_path.display()));

        let mut found_home = false;
        for line in cfg.lines() {
            if let Some(home_bin) = line.strip_prefix("home = ") {
                let base = Path::new(home_bin.trim()).parent().unwrap();
                unsafe {
                    std::env::set_var("PYTHONHOME", base);
                    std::env::set_var("VIRTUAL_ENV", &venv);
                }
                found_home = true;
            } else if let Some(ver) = line.strip_prefix("version_info = ") {
                // e.g. "3.11.10" → "3.11"
                let parts: Vec<&str> = ver.trim().splitn(3, '.').collect();
                if parts.len() >= 2 {
                    let _ = PYTHON_VERSION.set(format!("{}.{}", parts[0], parts[1]));
                }
            }
        }
        assert!(found_home, "pyvenv.cfg missing `home` key");
    });
}

/// Parse only the Python version from pyvenv.cfg (for the PYTHONHOME-already-set case).
fn parse_python_version() {
    if PYTHON_VERSION.get().is_some() {
        return;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let cfg_path = workspace_root.join(".venv/pyvenv.cfg");
    if let Ok(cfg) = std::fs::read_to_string(&cfg_path) {
        for line in cfg.lines() {
            if let Some(ver) = line.strip_prefix("version_info = ") {
                let parts: Vec<&str> = ver.trim().splitn(3, '.').collect();
                if parts.len() >= 2 {
                    let _ = PYTHON_VERSION.set(format!("{}.{}", parts[0], parts[1]));
                }
                return;
            }
        }
    }
}

/// Returns the cached `"X.Y"` Python version string (e.g. `"3.11"`).
/// Falls back to `"3.11"` if pyvenv.cfg couldn't be parsed.
pub fn python_version() -> &'static str {
    PYTHON_VERSION.get().map_or("3.11", String::as_str)
}

/// Self-contained test server: writes Python app to tempdir, discovers routes,
/// builds router, serves over TCP. Shut down via [`TestServer::stop`] or `Drop`.
pub struct TestServer {
    base_url: String,
    port: u16,
    client: reqwest::Client,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    _lifespan_guard: Option<LifespanGuard>,
    _tmp_dir: tempfile::TempDir,
    /// Per-test event loop (only for `start_with_scheduler`; shared loop tests use `None`).
    _event_loop: Option<EventLoop>,
}

impl TestServer {
    /// Start a test server running the given Python FastAPI app source.
    ///
    /// The module name is derived from the test to avoid import collisions
    /// when multiple tests run in the same process.
    pub async fn start(python_app: &str, module_name: &str) -> Self {
        // Limit concurrent event loops to avoid GIL starvation.
        ensure_tracing();
        ensure_python_env();
        with_py(|_| {});

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let app_file = tmp_dir.path().join(format!("{module_name}.py"));
        std::fs::write(&app_file, python_app).unwrap();

        let tmp_path = tmp_dir.path().to_path_buf();
        let module = module_name.to_owned();

        let (loop_handle, routes) = {
            let _guard = DISCOVERY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let loop_handle = shared_event_loop_handle();
            let routes = with_py(|py| {
                let sys = py.import("sys").unwrap();
                let path = sys.getattr("path").unwrap();

                let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
                let site_packages = manifest_dir
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(format!(
                        ".venv/lib/python{}/site-packages",
                        python_version()
                    ));
                path.call_method1("insert", (0, site_packages.to_str().unwrap()))
                    .unwrap();
                path.call_method1("insert", (0, tmp_path.to_str().unwrap()))
                    .unwrap();

                // Remove stale cached module to force fresh import.
                let modules = sys.getattr("modules").unwrap();
                let _ = modules.call_method1("pop", (&*module, py.None()));

                let app_module = AppModule::new(&module).unwrap();
                let (routes, _manifest) = discovery::discover_and_bind(py, &app_module).unwrap();
                routes
            });
            (loop_handle, routes)
        };

        assert!(!routes.is_empty(), "no routes discovered in {module_name}");

        // Run ASGI lifespan startup if any route has a FastAPI app reference.
        let lifespan_guard = if routes.iter().any(|r| r.fastapi_app.is_some()) {
            let app_ref = routes
                .iter()
                .find_map(|r| r.fastapi_app.as_ref())
                .unwrap()
                .inner();
            match run_lifespan_startup(app_ref, &loop_handle).await {
                Ok(guard) => Some(guard),
                Err(e) => {
                    tracing::debug!(error = %e, "lifespan startup skipped (no lifespan handler)");
                    None
                }
            }
        } else {
            None
        };

        // Bind listener first so the scope template gets the real server address.
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let addr = listener.local_addr();

        let scope_interns = with_py(crate::bridge::asgi::ScopeInterns::new);
        let scope_interns = Arc::new(scope_interns);
        let scope_template = pyo3::Python::attach(|py| {
            let app_ref = routes.iter().find_map(|r| r.fastapi_app.as_ref());
            crate::bridge::context_pool::build_scope_template(
                py,
                &scope_interns,
                app_ref.map(|a| a.inner()),
                addr,
            )
            .unwrap()
        });
        let receive_template = pyo3::Python::attach(|py| {
            crate::bridge::context_pool::build_receive_template(py).unwrap()
        });
        let app_state = Arc::new(AppState {
            max_body_limit: BodyLimit::DEFAULT,
            loop_handle,
            scope_interns,
            scope_template: Arc::new(scope_template),
            receive_template: Arc::new(receive_template),
        });

        let router = build_router(routes, app_state, addr);
        let router = wrap_layers(router, None);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            listener
                .serve(router, async {
                    shutdown_rx.await.ok();
                })
                .await
                .unwrap();
        });

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            port: addr.port(),
            client: reqwest::Client::new(),
            shutdown_tx: Some(shutdown_tx),
            server_handle: Some(server_handle),
            _lifespan_guard: lifespan_guard,
            _tmp_dir: tmp_dir,
            _event_loop: None,
        }
    }

    /// Start a test server that goes through the full manifest roundtrip:
    /// discover → serialize to JSON → deserialize → bind from manifest → serve.
    ///
    /// Tests the production serving path (manifest-based) end-to-end.
    pub async fn start_from_manifest(python_app: &str, module_name: &str) -> Self {
        ensure_tracing();
        ensure_python_env();
        with_py(|_| {});

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let app_file = tmp_dir.path().join(format!("{module_name}.py"));
        std::fs::write(&app_file, python_app).unwrap();

        let tmp_path = tmp_dir.path().to_path_buf();
        let module = module_name.to_owned();
        let manifest_path = tmp_dir.path().join("manifest.json");

        let (loop_handle, app_module, loaded_manifest) = {
            let _guard = DISCOVERY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let loop_handle = shared_event_loop_handle();

            // Phase 1: Discover routes and save as manifest JSON.
            let app_module = with_py(|py| {
                let sys = py.import("sys").unwrap();
                let path = sys.getattr("path").unwrap();

                let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
                let site_packages = manifest_dir
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(format!(
                        ".venv/lib/python{}/site-packages",
                        python_version()
                    ));
                path.call_method1("insert", (0, site_packages.to_str().unwrap()))
                    .unwrap();
                path.call_method1("insert", (0, tmp_path.to_str().unwrap()))
                    .unwrap();

                // Remove stale cached module to force fresh import.
                let modules = sys.getattr("modules").unwrap();
                let _ = modules.call_method1("pop", (&*module, py.None()));

                let app_module = AppModule::new(&module).unwrap();
                let (_routes, mut manifest) =
                    discovery::discover_and_bind(py, &app_module).unwrap();

                manifest.meta = Some(crate::route::ManifestMeta {
                    apx_version: env!("CARGO_PKG_VERSION").to_owned(),
                    python_version: python_version().to_owned(),
                    fastapi_version: None,
                    build_timestamp: "2025-01-01T00:00:00Z".to_owned(),
                    app_module: app_module.clone(),
                    source_hash: None,
                });

                crate::manifest::save(&manifest, &manifest_path).unwrap();
                app_module
            });

            let loaded_manifest = crate::manifest::load(&manifest_path).unwrap();
            (loop_handle, app_module, loaded_manifest)
        };

        // Phase 2: Load manifest from JSON and bind routes (manifest serving path).
        let routes = with_py(|py| {
            discovery::bind::bind_routes_from_manifest(py, &loaded_manifest, &app_module).unwrap()
        });

        assert!(
            !routes.is_empty(),
            "no routes in manifest for {module_name}"
        );

        // Run ASGI lifespan startup.
        let lifespan_guard = if routes.iter().any(|r| r.fastapi_app.is_some()) {
            let app_ref = routes
                .iter()
                .find_map(|r| r.fastapi_app.as_ref())
                .unwrap()
                .inner();
            match run_lifespan_startup(app_ref, &loop_handle).await {
                Ok(guard) => Some(guard),
                Err(e) => {
                    tracing::debug!(error = %e, "lifespan startup skipped");
                    None
                }
            }
        } else {
            None
        };

        // Bind listener first so the scope template gets the real server address.
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let addr = listener.local_addr();

        let scope_interns = with_py(crate::bridge::asgi::ScopeInterns::new);
        let scope_interns = Arc::new(scope_interns);
        let scope_template = pyo3::Python::attach(|py| {
            crate::bridge::context_pool::build_scope_template(py, &scope_interns, None, addr)
                .unwrap()
        });
        let receive_template = pyo3::Python::attach(|py| {
            crate::bridge::context_pool::build_receive_template(py).unwrap()
        });
        let app_state = Arc::new(AppState {
            max_body_limit: loaded_manifest.max_body_limit,
            loop_handle,
            scope_interns,
            scope_template: Arc::new(scope_template),
            receive_template: Arc::new(receive_template),
        });

        let router = build_router(routes, app_state, addr);
        let router = wrap_layers(router, None);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            listener
                .serve(router, async {
                    shutdown_rx.await.ok();
                })
                .await
                .unwrap();
        });

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            port: addr.port(),
            client: reqwest::Client::new(),
            shutdown_tx: Some(shutdown_tx),
            server_handle: Some(server_handle),
            _lifespan_guard: lifespan_guard,
            _tmp_dir: tmp_dir,
            _event_loop: None,
        }
    }

    /// Start a test server with `LoopPolicy::RustNative` (the Rust scheduler).
    ///
    /// Creates an isolated event loop per test — does NOT use the shared loop.
    /// Used by scheduler compatibility tests to find asyncio/anyio
    /// incompatibilities when the Rust driver replaces asyncio.Task.
    pub async fn start_with_scheduler(python_app: &str, module_name: &str) -> Self {
        ensure_tracing();
        ensure_python_env();
        with_py(|_| {});

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let app_file = tmp_dir.path().join(format!("{module_name}.py"));
        std::fs::write(&app_file, python_app).unwrap();

        let tmp_path = tmp_dir.path().to_path_buf();
        let module = module_name.to_owned();

        let (event_loop, loop_handle, routes) = {
            let _guard = DISCOVERY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let event_loop =
                EventLoop::start_with(crate::event_loop::core::LoopPolicy::RustNative).unwrap();
            let loop_handle = event_loop.handle().unwrap();
            let routes = with_py(|py| {
                let sys = py.import("sys").unwrap();
                let path = sys.getattr("path").unwrap();

                let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
                let site_packages = manifest_dir
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(format!(
                        ".venv/lib/python{}/site-packages",
                        python_version()
                    ));
                path.call_method1("insert", (0, site_packages.to_str().unwrap()))
                    .unwrap();
                path.call_method1("insert", (0, tmp_path.to_str().unwrap()))
                    .unwrap();

                let modules = sys.getattr("modules").unwrap();
                let _ = modules.call_method1("pop", (&*module, py.None()));

                let app_module = AppModule::new(&module).unwrap();
                let (routes, _manifest) = discovery::discover_and_bind(py, &app_module).unwrap();
                routes
            });
            (event_loop, loop_handle, routes)
        };

        assert!(!routes.is_empty(), "no routes discovered in {module_name}");

        let lifespan_guard = if routes.iter().any(|r| r.fastapi_app.is_some()) {
            let app_ref = routes
                .iter()
                .find_map(|r| r.fastapi_app.as_ref())
                .unwrap()
                .inner();
            match run_lifespan_startup(app_ref, &loop_handle).await {
                Ok(guard) => Some(guard),
                Err(e) => {
                    tracing::debug!(error = %e, "lifespan startup skipped");
                    None
                }
            }
        } else {
            None
        };

        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let addr = listener.local_addr();

        let scope_interns = with_py(crate::bridge::asgi::ScopeInterns::new);
        let scope_interns = Arc::new(scope_interns);
        let scope_template = pyo3::Python::attach(|py| {
            let app_ref = routes.iter().find_map(|r| r.fastapi_app.as_ref());
            crate::bridge::context_pool::build_scope_template(
                py,
                &scope_interns,
                app_ref.map(|a| a.inner()),
                addr,
            )
            .unwrap()
        });
        let receive_template = pyo3::Python::attach(|py| {
            crate::bridge::context_pool::build_receive_template(py).unwrap()
        });
        let app_state = Arc::new(AppState {
            max_body_limit: BodyLimit::DEFAULT,
            loop_handle,
            scope_interns,
            scope_template: Arc::new(scope_template),
            receive_template: Arc::new(receive_template),
        });

        let router = build_router(routes, app_state, addr);
        let router = wrap_layers(router, None);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            listener
                .serve(router, async {
                    shutdown_rx.await.ok();
                })
                .await
                .unwrap();
        });

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            port: addr.port(),
            client: reqwest::Client::new(),
            shutdown_tx: Some(shutdown_tx),
            server_handle: Some(server_handle),
            _lifespan_guard: lifespan_guard,
            _tmp_dir: tmp_dir,
            _event_loop: Some(event_loop),
        }
    }

    /// GET request, return (status_code, json_body).
    pub async fn get(&self, path: &str) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// GET with custom headers.
    pub async fn get_with_headers(
        &self,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (u16, serde_json::Value) {
        let mut req = self.client.get(format!("{}{path}", self.base_url));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// POST with JSON body, return (status_code, json_body).
    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// POST with raw string body and custom content-type.
    pub async fn post_raw(
        &self,
        path: &str,
        body: &str,
        content_type: &str,
    ) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
            .header("content-type", content_type)
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// GET returning raw response text (for streaming).
    pub async fn get_text(&self, path: &str) -> (u16, String) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        (status, text)
    }

    /// PUT with JSON body.
    pub async fn put_json(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .put(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// DELETE request.
    pub async fn delete(&self, path: &str) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .delete(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// PATCH with JSON body.
    pub async fn patch_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .patch(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// POST with no body.
    pub async fn post_empty(&self, path: &str) -> (u16, serde_json::Value) {
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// POST with form-urlencoded body.
    pub async fn post_form(&self, path: &str, fields: &[(&str, &str)]) -> (u16, serde_json::Value) {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// POST with multipart form-data (single file field).
    pub async fn post_multipart(
        &self,
        path: &str,
        field_name: &str,
        file_name: &str,
        file_bytes: Vec<u8>,
    ) -> (u16, serde_json::Value) {
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_owned())
            .mime_str("application/octet-stream")
            .unwrap();
        let form = reqwest::multipart::Form::new().part(field_name.to_owned(), part);
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
            .multipart(form)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    /// GET returning raw response (status, headers, body bytes).
    pub async fn get_raw(&self, path: &str) -> (u16, reqwest::header::HeaderMap, bytes::Bytes) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.bytes().await.unwrap_or_default();
        (status, headers, body)
    }

    /// GET with redirect policy disabled — returns the redirect response itself.
    pub async fn get_no_redirect(&self, path: &str) -> (u16, reqwest::header::HeaderMap) {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        (status, headers)
    }

    /// Shut down the server.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
