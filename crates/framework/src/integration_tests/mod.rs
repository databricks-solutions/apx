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
mod middleware;
mod params;
mod responses;
mod routing;

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
use std::sync::Arc;

// ── Test server harness ─────────────────────────────────────────────────

/// Ensure `PYTHONHOME` is set so the embedded interpreter can find its stdlib.
#[expect(unsafe_code, reason = "env::set_var required for Python interpreter")]
fn ensure_python_home() {
    if std::env::var("PYTHONHOME").is_ok() {
        return;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let venv = workspace_root.join(".venv");
    let cfg_path = venv.join("pyvenv.cfg");
    let cfg = std::fs::read_to_string(&cfg_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", cfg_path.display()));
    for line in cfg.lines() {
        if let Some(home_bin) = line.strip_prefix("home = ") {
            let base = Path::new(home_bin.trim()).parent().unwrap();
            unsafe {
                std::env::set_var("PYTHONHOME", base);
                std::env::set_var("VIRTUAL_ENV", &venv);
            }
            return;
        }
    }
    panic!("pyvenv.cfg missing `home` key");
}

/// Self-contained test server: writes Python app to tempdir, discovers routes,
/// builds router, serves over TCP. Shut down via [`TestServer::stop`] or `Drop`.
pub struct TestServer {
    base_url: String,
    port: u16,
    client: reqwest::Client,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    event_loop: EventLoop,
    _lifespan_guard: Option<LifespanGuard>,
    _tmp_dir: tempfile::TempDir,
}

impl TestServer {
    /// Start a test server running the given Python FastAPI app source.
    ///
    /// The module name is derived from the test to avoid import collisions
    /// when multiple tests run in the same process.
    pub async fn start(python_app: &str, module_name: &str) -> Self {
        ensure_python_home();
        with_py(|_| {});

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let app_file = tmp_dir.path().join(format!("{module_name}.py"));
        std::fs::write(&app_file, python_app).unwrap();

        let event_loop = EventLoop::start().unwrap();

        let tmp_path = tmp_dir.path().to_path_buf();
        let module = module_name.to_owned();
        let routes = with_py(|py| {
            let sys = py.import("sys").unwrap();
            let path = sys.getattr("path").unwrap();

            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let site_packages = manifest_dir
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(".venv/lib/python3.11/site-packages");
            path.call_method1("insert", (0, site_packages.to_str().unwrap()))
                .unwrap();
            path.call_method1("insert", (0, tmp_path.to_str().unwrap()))
                .unwrap();

            let app_module = AppModule::new(&module).unwrap();
            let (routes, _manifest) = discovery::discover_and_bind(py, &app_module).unwrap();
            routes
        });

        assert!(!routes.is_empty(), "no routes discovered in {module_name}");

        let loop_handle = event_loop.handle();

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

        let app_state = Arc::new(AppState {
            max_body_limit: BodyLimit::DEFAULT,
            loop_handle,
        });
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let addr = listener.local_addr();

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
            event_loop,
            _lifespan_guard: lifespan_guard,
            _tmp_dir: tmp_dir,
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

    /// Shut down the server and event loop.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.await;
        }
        self.event_loop.stop();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.event_loop.stop();
    }
}
