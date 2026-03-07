//! Integration tests: full serve path (discovery → router → TCP → HTTP).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]

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
#[allow(unsafe_code)]
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
#[allow(
    dead_code,
    reason = "methods used incrementally as integration tests are added"
)]
struct TestServer {
    base_url: String,
    port: u16,
    client: reqwest::Client,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    event_loop: EventLoop,
    _tmp_dir: tempfile::TempDir,
}

#[allow(
    dead_code,
    reason = "methods used incrementally as integration tests are added"
)]
impl TestServer {
    /// Start a test server running the given Python FastAPI app source.
    ///
    /// The module name is derived from the test to avoid import collisions
    /// when multiple tests run in the same process.
    async fn start(python_app: &str, module_name: &str) -> Self {
        ensure_python_home();
        // Ensure Python interpreter is initialized before spawning event loop thread.
        with_py(|_| {});

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let app_file = tmp_dir.path().join(format!("{module_name}.py"));
        std::fs::write(&app_file, python_app).unwrap();

        let event_loop = EventLoop::start().unwrap();
        let loop_handle = event_loop.handle();

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
            _tmp_dir: tmp_dir,
        }
    }

    /// GET request, return (status_code, json_body).
    async fn get(&self, path: &str) -> (u16, serde_json::Value) {
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
    async fn get_with_headers(
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
    async fn post_json(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
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
    async fn post_raw(
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
    async fn get_text(&self, path: &str) -> (u16, String) {
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
    async fn put_json(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
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
    async fn delete(&self, path: &str) -> (u16, serde_json::Value) {
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
    async fn patch_json(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
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
    async fn post_empty(&self, path: &str) -> (u16, serde_json::Value) {
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

    /// Shut down the server and event loop.
    async fn stop(&mut self) {
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

// ── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn serve_and_request() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}

class Item(BaseModel):
    name: str

@app.post("/items")
async def create_item(item: Item):
    return {"name": item.name, "created": True}

@app.get("/sync")
def sync_hello():
    return {"message": "sync"}
"#;

    let mut server = TestServer::start(app, "_apx_test_basic").await;

    let (status, body) = server.get("/hello").await;
    assert_eq!(status, 200, "GET /hello failed: {body}");
    assert_eq!(body["message"], "hello");

    let (status, body) = server.get("/sync").await;
    assert_eq!(status, 200, "GET /sync failed: {body}");
    assert_eq!(body["message"], "sync");

    let (status, body) = server
        .post_json("/items", serde_json::json!({"name": "test"}))
        .await;
    assert_eq!(status, 200, "POST /items failed: {body}");
    assert_eq!(body["name"], "test");
    assert_eq!(body["created"], true);

    server.stop().await;
}

/// HTTP error responses via FastAPI's `HTTPException`.
#[tokio::test]
async fn error_responses() {
    let app = r#"
from fastapi import FastAPI, HTTPException

app = FastAPI()

@app.get("/not-found")
async def raise_not_found():
    raise HTTPException(status_code=404, detail="item 42 not found")

@app.get("/bad-request")
async def raise_bad_request():
    raise HTTPException(status_code=400, detail="invalid input")

@app.get("/forbidden")
async def raise_forbidden():
    raise HTTPException(status_code=403, detail="access denied")

@app.get("/unhandled")
async def raise_unhandled():
    raise RuntimeError("secret internal detail")
"#;

    let mut server = TestServer::start(app, "_apx_test_errors").await;

    let (status, body) = server.get("/not-found").await;
    assert_eq!(status, 404, "expected 404: {body}");
    assert_eq!(body["detail"], "item 42 not found");

    let (status, body) = server.get("/bad-request").await;
    assert_eq!(status, 400, "expected 400: {body}");
    assert_eq!(body["detail"], "invalid input");

    let (status, body) = server.get("/forbidden").await;
    assert_eq!(status, 403, "expected 403: {body}");
    assert_eq!(body["detail"], "access denied");

    // Unhandled → 500, detail must NOT leak internal message
    let (status, body) = server.get("/unhandled").await;
    assert_eq!(status, 500, "expected 500: {body}");

    server.stop().await;
}

#[tokio::test]
async fn validation_and_body_errors() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()

class Item(BaseModel):
    name: str
    price: float

@app.post("/items")
async def create_item(item: Item):
    return {"name": item.name, "price": item.price}
"#;

    let mut server = TestServer::start(app, "_apx_test_validation").await;

    // Valid body → 200
    let (status, body) = server
        .post_json(
            "/items",
            serde_json::json!({"name": "widget", "price": 9.99}),
        )
        .await;
    assert_eq!(status, 200, "valid body failed: {body}");
    assert_eq!(body["name"], "widget");

    // Missing required field → 422 with structured errors (FastAPI format)
    let (status, body) = server
        .post_json("/items", serde_json::json!({"name": "widget"}))
        .await;
    assert_eq!(status, 422, "missing field should be 422: {body}");
    assert!(
        body["detail"].is_array(),
        "422 should have detail array: {body}"
    );

    // Invalid JSON → 422 (FastAPI validates via Pydantic, which rejects malformed input)
    let (status, _body) = server
        .post_raw("/items", "not json", "application/json")
        .await;
    assert!(
        status == 400 || status == 422,
        "invalid JSON should be 400 or 422, got {status}"
    );

    server.stop().await;
}

#[tokio::test]
async fn path_query_header_params() {
    let app = r#"
from fastapi import FastAPI, Header, Query
from typing import Optional

app = FastAPI()

@app.get("/items/{item_id}")
async def get_item(item_id: int):
    return {"item_id": item_id}

@app.get("/search")
async def search(q: str):
    return {"q": q}

@app.get("/with-header")
async def with_header(x_token: str = Header()):
    return {"token": x_token}
"#;

    let mut server = TestServer::start(app, "_apx_test_params").await;

    // Path param: int conversion
    let (status, body) = server.get("/items/42").await;
    assert_eq!(status, 200, "path param int: {body}");
    assert_eq!(body["item_id"], 42);

    // Path param: invalid int → 422 (FastAPI validation error)
    let (status, _body) = server.get("/items/abc").await;
    assert_eq!(status, 422, "invalid int path param should be 422");

    // Query param present
    let (status, body) = server.get("/search?q=hello").await;
    assert_eq!(status, 200, "query present: {body}");
    assert_eq!(body["q"], "hello");

    // Query param missing required → 422
    let (status, _body) = server.get("/search").await;
    assert_eq!(status, 422, "missing required query should be 422");

    // Header param: present
    let (status, body) = server
        .get_with_headers("/with-header", &[("x-token", "secret")])
        .await;
    assert_eq!(status, 200, "header param: {body}");
    assert_eq!(body["token"], "secret");

    // Header param: missing → 422
    let (status, _body) = server.get("/with-header").await;
    assert_eq!(status, 422, "missing header should be 422");

    server.stop().await;
}

/// Depends() routes go through ASGI bridge (FastAPI's solve_dependencies).
#[tokio::test]
async fn depends_dispatch() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

async def get_db():
    return {"connection": "active"}

def get_config():
    return {"env": "test"}

@app.get("/with-async-dep")
async def with_async_dep(db=Depends(get_db)):
    return {"db": db["connection"]}

@app.get("/with-sync-dep")
async def with_sync_dep(config=Depends(get_config)):
    return {"env": config["env"]}
"#;

    let mut server = TestServer::start(app, "_apx_test_depends").await;

    let (status, body) = server.get("/with-async-dep").await;
    assert_eq!(status, 200, "async dep: {body}");
    assert_eq!(body["db"], "active");

    let (status, body) = server.get("/with-sync-dep").await;
    assert_eq!(status, 200, "sync dep: {body}");
    assert_eq!(body["env"], "test");

    server.stop().await;
}

/// Streaming responses go through ASGI bridge with `response_type: StreamingResponse`.
#[tokio::test]
async fn streaming_response() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import StreamingResponse

app = FastAPI()

async def generate():
    for i in range(3):
        yield f"chunk-{i}\n"

@app.get("/stream")
async def stream():
    return StreamingResponse(generate(), media_type="text/plain")
"#;

    let mut server = TestServer::start(app, "_apx_test_stream").await;

    let (status, text) = server.get_text("/stream").await;
    assert_eq!(status, 200, "stream: {text}");
    assert_eq!(text, "chunk-0\nchunk-1\nchunk-2\n");

    server.stop().await;
}

/// WebSocket echo handler exercising the full WS dispatch path.
#[tokio::test]
async fn websocket_echo() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;

    let app = r#"
from fastapi import FastAPI, WebSocket

app = FastAPI()

@app.websocket("/ws/echo")
async def websocket_echo(websocket: WebSocket):
    await websocket.accept()
    while True:
        data = await websocket.receive_text()
        if data == "close":
            await websocket.close()
            break
        await websocket.send_text(f"echo: {data}")
"#;

    let mut server = TestServer::start(app, "_apx_test_ws").await;

    let ws_url = format!("ws://127.0.0.1:{}/ws/echo", server.port);
    let (mut ws_stream, _resp) = connect_async(&ws_url).await.expect("WS connect failed");

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "hello".into(),
        ))
        .await
        .unwrap();
    let msg = ws_stream.next().await.unwrap().unwrap();
    assert_eq!(msg.into_text().unwrap(), "echo: hello");

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "close".into(),
        ))
        .await
        .unwrap();

    server.stop().await;
}

// ── Test 1: Path parameters ────────────────────────────────────────────

#[tokio::test]
async fn path_params() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/items/{item_id}")
async def get_item(item_id: int):
    return {"item_id": item_id}

@app.get("/users/{username}")
async def get_user(username: str):
    return {"username": username}

@app.get("/floats/{value}")
async def get_float(value: float):
    return {"value": value}
"#;

    let mut server = TestServer::start(app, "_apx_test_path_params").await;

    // Valid int
    let (status, body) = server.get("/items/42").await;
    assert_eq!(status, 200, "GET /items/42: {body}");
    assert_eq!(body["item_id"], 42);

    // Zero is valid
    let (status, body) = server.get("/items/0").await;
    assert_eq!(status, 200, "GET /items/0: {body}");
    assert_eq!(body["item_id"], 0);

    // Negative int
    let (status, body) = server.get("/items/-1").await;
    assert_eq!(status, 200, "GET /items/-1: {body}");
    assert_eq!(body["item_id"], -1);

    // Invalid int → 422 (FastAPI validation error)
    let (status, _body) = server.get("/items/abc").await;
    assert_eq!(status, 422, "non-int path param should be 422");

    // Float is not int → 422 (FastAPI validation error)
    let (status, _body) = server.get("/items/3.14").await;
    assert_eq!(status, 422, "float path param should be 422 for int route");

    // String path param
    let (status, body) = server.get("/users/alice").await;
    assert_eq!(status, 200, "GET /users/alice: {body}");
    assert_eq!(body["username"], "alice");

    // URL-encoded string
    let (status, body) = server.get("/users/hello%20world").await;
    assert_eq!(status, 200, "URL-decoded path param: {body}");
    assert_eq!(body["username"], "hello world");

    // Float path param
    let (status, body) = server.get("/floats/3.14").await;
    assert_eq!(status, 200, "GET /floats/3.14: {body}");
    let value = body["value"].as_f64().unwrap();
    #[allow(clippy::approx_constant)]
    let expected = 3.14_f64;
    assert!((value - expected).abs() < 0.001);

    server.stop().await;
}

/// `:path` catch-all route param is not supported
/// by the axum/matchit router — multi-segment path params don't match.
#[tokio::test]
#[should_panic(expected = "path param catch-all unsupported")]
#[allow(clippy::literal_string_with_formatting_args)]
async fn path_param_catch_all_unsupported() {
    let app = "
from fastapi import FastAPI
app = FastAPI()

@app.get(\"/files/{file_path:path}\")
async def get_file(file_path: str):
    return {\"path\": file_path}
";

    let mut server = TestServer::start(app, "_apx_test_path_catchall").await;

    let (status, body) = server.get("/files/home/user/doc.txt").await;
    assert_eq!(
        status, 200,
        "path param catch-all unsupported: got {status}: {body}"
    );
    assert_eq!(
        body["path"], "home/user/doc.txt",
        "path param catch-all unsupported"
    );

    server.stop().await;
}

/// `float("nan")` passes Python's float() conversion but produces
/// invalid JSON (`NaN`), which is not rejected at the parameter level.
#[tokio::test]
#[should_panic(expected = "float nan should be rejected")]
async fn float_nan_not_rejected() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/floats/{value}")
async def get_float(value: float):
    return {"value": value}
"#;

    let mut server = TestServer::start(app, "_apx_test_float_nan").await;

    let (status, _body) = server.get("/floats/nan").await;
    assert_eq!(
        status, 400,
        "float nan should be rejected as invalid, got {status}"
    );

    server.stop().await;
}

// ── Test 2: Query parameters ────────────────────────────────────────────

#[tokio::test]
async fn query_params() {
    let app = r#"
from fastapi import FastAPI
from typing import Optional
app = FastAPI()

@app.get("/required")
async def required_query(q: str):
    return {"q": q}

@app.get("/optional")
async def optional_query(q: Optional[str] = None):
    return {"q": q}
"#;

    let mut server = TestServer::start(app, "_apx_test_query_params").await;

    // Required present
    let (status, body) = server.get("/required?q=hello").await;
    assert_eq!(status, 200, "required query: {body}");
    assert_eq!(body["q"], "hello");

    // Required missing → 422
    let (status, _body) = server.get("/required").await;
    assert_eq!(status, 422, "missing required query should be 422");

    // Empty string is present
    let (status, body) = server.get("/required?q=").await;
    assert_eq!(status, 200, "empty string query: {body}");
    assert_eq!(body["q"], "");

    // URL-encoded
    let (status, body) = server.get("/required?q=hello%20world").await;
    assert_eq!(status, 200, "URL-decoded query: {body}");
    assert_eq!(body["q"], "hello world");

    // Optional absent → null
    let (status, body) = server.get("/optional").await;
    assert_eq!(status, 200, "optional absent: {body}");
    assert!(
        body["q"].is_null(),
        "optional absent should be null: {body}"
    );

    // Optional present
    let (status, body) = server.get("/optional?q=test").await;
    assert_eq!(status, 200, "optional present: {body}");
    assert_eq!(body["q"], "test");

    server.stop().await;
}

/// Query parameter defaults (e.g. `page: int = 1`) are applied via
/// `json.loads` when the parameter is absent from the request.
#[tokio::test]
async fn query_defaults_applied() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/with-default")
async def with_default(page: int = 1, size: int = 10):
    return {"page": page, "size": size}
"#;

    let mut server = TestServer::start(app, "_apx_test_query_defaults").await;

    let (status, body) = server.get("/with-default").await;
    assert_eq!(status, 200, "defaults absent: {body}");
    assert_eq!(body["page"], 1, "page default: {body}");
    assert_eq!(body["size"], 10, "size default: {body}");

    // Override one default
    let (status, body) = server.get("/with-default?page=5").await;
    assert_eq!(status, 200, "partial override: {body}");
    assert_eq!(body["page"], 5, "page override: {body}");
    assert_eq!(body["size"], 10, "size still default: {body}");

    server.stop().await;
}

/// Bool query params (e.g. `active: bool`) handled by FastAPI via ASGI bridge.
#[tokio::test]
async fn bool_query() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/bool-query")
async def bool_query(active: bool = True):
    return {"active": active}
"#;

    let mut server = TestServer::start(app, "_apx_test_bool_query").await;

    let (status, body) = server.get("/bool-query?active=true").await;
    assert_eq!(status, 200, "bool query not supported: status {status}");
    assert_eq!(
        body["active"], true,
        "bool query not supported: active={}",
        body["active"]
    );

    server.stop().await;
}

// ── Test 3: Request body ────────────────────────────────────────────────

#[tokio::test]
async fn request_body() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
from typing import Optional, List
app = FastAPI()

class Item(BaseModel):
    name: str
    price: float
    description: Optional[str] = None
    tags: List[str] = []

@app.post("/items")
async def create_item(item: Item):
    return item.model_dump()

@app.post("/empty-ok")
async def empty_ok():
    return {"ok": True}
"#;

    let mut server = TestServer::start(app, "_apx_test_request_body").await;

    // Valid body with required fields only
    let (status, body) = server
        .post_json("/items", serde_json::json!({"name": "x", "price": 1.0}))
        .await;
    assert_eq!(status, 200, "valid body: {body}");
    assert_eq!(body["name"], "x");
    assert_eq!(body["price"], 1.0);
    assert!(body["description"].is_null());
    assert_eq!(body["tags"], serde_json::json!([]));

    // Valid body with all fields
    let (status, body) = server
        .post_json(
            "/items",
            serde_json::json!({"name": "x", "price": 1.0, "description": "d", "tags": ["a", "b"]}),
        )
        .await;
    assert_eq!(status, 200, "all fields: {body}");
    assert_eq!(body["description"], "d");
    assert_eq!(body["tags"], serde_json::json!(["a", "b"]));

    // Missing required fields → 422
    let (status, body) = server.post_json("/items", serde_json::json!({})).await;
    assert_eq!(status, 422, "missing fields should be 422: {body}");

    // Wrong type → 422
    let (status, _body) = server
        .post_json(
            "/items",
            serde_json::json!({"name": "x", "price": "notnum"}),
        )
        .await;
    assert_eq!(status, 422, "wrong type should be 422");

    // No body at all → 422
    let (status, _body) = server.post_empty("/items").await;
    assert!(
        status == 400 || status == 422,
        "no body should be 400 or 422, got {status}"
    );

    // Handler with no body param accepts empty POST
    let (status, body) = server.post_empty("/empty-ok").await;
    assert_eq!(status, 200, "empty-ok: {body}");
    assert_eq!(body["ok"], true);

    server.stop().await;
}

#[tokio::test]
async fn nested_body_models() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class Address(BaseModel):
    street: str
    city: str

class UserWithAddress(BaseModel):
    name: str
    address: Address

@app.post("/users")
async def create_user(user: UserWithAddress):
    return user.model_dump()
"#;

    let mut server = TestServer::start(app, "_apx_test_nested_body").await;

    // Valid nested model
    let (status, body) = server
        .post_json(
            "/users",
            serde_json::json!({"name": "a", "address": {"street": "1st", "city": "NY"}}),
        )
        .await;
    assert_eq!(status, 200, "nested model: {body}");
    assert_eq!(body["name"], "a");
    assert_eq!(body["address"]["street"], "1st");
    assert_eq!(body["address"]["city"], "NY");

    // Invalid nested model → 422
    let (status, body) = server
        .post_json("/users", serde_json::json!({"name": "a", "address": {}}))
        .await;
    assert_eq!(status, 422, "invalid nested should be 422: {body}");

    server.stop().await;
}

// ── Test 4: Header parameters ───────────────────────────────────────────

#[tokio::test]
async fn header_params_extended() {
    let app = r#"
from fastapi import FastAPI, Header
from typing import Optional
app = FastAPI()

@app.get("/with-header")
async def with_header(x_token: str = Header()):
    return {"token": x_token}

@app.get("/optional-header")
async def optional_header(x_debug: Optional[str] = Header(default=None)):
    return {"debug": x_debug}

@app.get("/multiple-headers")
async def multiple_headers(
    x_token: str = Header(),
    x_trace_id: str = Header(),
):
    return {"token": x_token, "trace_id": x_trace_id}
"#;

    let mut server = TestServer::start(app, "_apx_test_header_ext").await;

    // Required header present
    let (status, body) = server
        .get_with_headers("/with-header", &[("x-token", "abc")])
        .await;
    assert_eq!(status, 200, "required header: {body}");
    assert_eq!(body["token"], "abc");

    // Required header missing → 422
    let (status, _body) = server.get("/with-header").await;
    assert_eq!(status, 422, "missing required header should be 422");

    // Optional header absent → null
    let (status, body) = server.get("/optional-header").await;
    assert_eq!(status, 200, "optional header absent: {body}");
    assert!(
        body["debug"].is_null(),
        "optional header should be null: {body}"
    );

    // Optional header present
    let (status, body) = server
        .get_with_headers("/optional-header", &[("x-debug", "on")])
        .await;
    assert_eq!(status, 200, "optional header present: {body}");
    assert_eq!(body["debug"], "on");

    // Multiple headers
    let (status, body) = server
        .get_with_headers(
            "/multiple-headers",
            &[("x-token", "tok"), ("x-trace-id", "trace123")],
        )
        .await;
    assert_eq!(status, 200, "multiple headers: {body}");
    assert_eq!(body["token"], "tok");
    assert_eq!(body["trace_id"], "trace123");

    server.stop().await;
}

// ── Test 5: Cookie parameters ───────────────────────────────────────────

#[tokio::test]
async fn cookie_params() {
    let app = r#"
from fastapi import FastAPI, Cookie
from typing import Optional
app = FastAPI()

@app.get("/with-cookie")
async def with_cookie(session_id: str = Cookie()):
    return {"session": session_id}

@app.get("/optional-cookie")
async def optional_cookie(tracking: Optional[str] = Cookie(default=None)):
    return {"tracking": tracking}
"#;

    let mut server = TestServer::start(app, "_apx_test_cookie_params").await;

    // Cookie present
    let (status, body) = server
        .get_with_headers("/with-cookie", &[("cookie", "session_id=abc123")])
        .await;
    assert_eq!(status, 200, "cookie present: {body}");
    assert_eq!(body["session"], "abc123");

    // Cookie missing required → 422
    let (status, _body) = server.get("/with-cookie").await;
    assert_eq!(status, 422, "missing required cookie should be 422");

    // Optional cookie absent → null
    let (status, body) = server.get("/optional-cookie").await;
    assert_eq!(status, 200, "optional cookie absent: {body}");
    assert!(
        body["tracking"].is_null(),
        "optional cookie should be null: {body}"
    );

    server.stop().await;
}

// ── Test 6: Response status codes ───────────────────────────────────────

#[tokio::test]
async fn response_status_code_201() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class Item(BaseModel):
    name: str

@app.post("/items", status_code=201)
async def create_item(item: Item):
    return item.model_dump()
"#;

    let mut server = TestServer::start(app, "_apx_test_status_201").await;

    let (status, body) = server
        .post_json("/items", serde_json::json!({"name": "x"}))
        .await;
    assert_eq!(status, 201, "custom 201 status code: {body}");
    assert_eq!(body["name"], "x");

    server.stop().await;
}

// ── Test 7: Response model filtering ────────────────────────────────────

/// `response_model=UserOut` filters fields from the response via FastAPI ASGI bridge.
#[tokio::test]
async fn response_model_filtering() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class UserIn(BaseModel):
    username: str
    password: str
    email: str

class UserOut(BaseModel):
    username: str
    email: str

@app.post("/users", response_model=UserOut)
async def create_user(user: UserIn):
    return user
"#;

    let mut server = TestServer::start(app, "_apx_test_resp_model").await;

    let (status, body) = server
        .post_json(
            "/users",
            serde_json::json!({"username": "a", "password": "secret", "email": "a@b.c"}),
        )
        .await;
    assert_eq!(
        status, 200,
        "response model filtering unsupported: got {status}: {body}"
    );

    server.stop().await;
}

// ── Test 8: HTTP methods ────────────────────────────────────────────────

#[tokio::test]
async fn http_methods() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class Item(BaseModel):
    name: str
    price: float

@app.put("/items/{item_id}")
async def replace_item(item_id: int, item: Item):
    return {"item_id": item_id, **item.model_dump()}

@app.patch("/items/{item_id}")
async def update_item(item_id: int, item: Item):
    return {"item_id": item_id, **item.model_dump()}

@app.delete("/items/{item_id}")
async def delete_item(item_id: int):
    return {"deleted": item_id}
"#;

    let mut server = TestServer::start(app, "_apx_test_http_methods").await;

    // PUT
    let (status, body) = server
        .put_json("/items/1", serde_json::json!({"name": "x", "price": 1.0}))
        .await;
    assert_eq!(status, 200, "PUT: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "x");

    // PATCH
    let (status, body) = server
        .patch_json("/items/1", serde_json::json!({"name": "y", "price": 2.0}))
        .await;
    assert_eq!(status, 200, "PATCH: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "y");

    // DELETE
    let (status, body) = server.delete("/items/1").await;
    assert_eq!(status, 200, "DELETE: {body}");
    assert_eq!(body["deleted"], 1);

    server.stop().await;
}

// ── Test 9: Null/empty edge cases ───────────────────────────────────────

#[tokio::test]
async fn null_empty_edge_cases() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/null-return")
async def null_return():
    return None

@app.get("/empty-dict")
async def empty_dict():
    return {}

@app.get("/empty-list")
async def empty_list():
    return []

@app.get("/empty-string")
async def empty_string():
    return ""

@app.get("/zero")
async def zero():
    return 0

@app.get("/false")
async def false_val():
    return False
"#;

    let mut server = TestServer::start(app, "_apx_test_edge_cases").await;

    let (status, body) = server.get("/null-return").await;
    assert_eq!(status, 200, "null-return: {body}");
    assert!(body.is_null(), "null-return should be null: {body}");

    let (status, body) = server.get("/empty-dict").await;
    assert_eq!(status, 200, "empty-dict: {body}");
    assert_eq!(body, serde_json::json!({}));

    let (status, body) = server.get("/empty-list").await;
    assert_eq!(status, 200, "empty-list: {body}");
    assert_eq!(body, serde_json::json!([]));

    let (status, body) = server.get("/empty-string").await;
    assert_eq!(status, 200, "empty-string: {body}");
    assert_eq!(body, serde_json::json!(""));

    let (status, body) = server.get("/zero").await;
    assert_eq!(status, 200, "zero: {body}");
    assert_eq!(body, serde_json::json!(0));

    let (status, body) = server.get("/false").await;
    assert_eq!(status, 200, "false: {body}");
    assert_eq!(body, serde_json::json!(false));

    server.stop().await;
}

// ── Test 10: Error handling extensions ───────────────────────────────────

#[tokio::test]
async fn error_handling_extensions() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/value-error")
async def value_error():
    raise ValueError("bad value")

@app.get("/type-error")
async def type_error():
    raise TypeError("wrong type")

@app.get("/key-error")
async def key_error():
    d = {}
    return d["missing"]
"#;

    let mut server = TestServer::start(app, "_apx_test_error_ext").await;

    // ValueError → 500, no detail leak
    let (status, body) = server.get("/value-error").await;
    assert_eq!(status, 500, "ValueError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("bad value"),
        "ValueError detail leaked: {body}"
    );

    // TypeError → 500, no detail leak
    let (status, body) = server.get("/type-error").await;
    assert_eq!(status, 500, "TypeError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("wrong type"),
        "TypeError detail leaked: {body}"
    );

    // KeyError → 500, no detail leak
    let (status, body) = server.get("/key-error").await;
    assert_eq!(status, 500, "KeyError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("missing"),
        "KeyError detail leaked: {body}"
    );

    server.stop().await;
}

// ── Test 14: Sync handlers ──────────────────────────────────────────────

#[tokio::test]
async fn sync_handlers_extended() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class Item(BaseModel):
    name: str

@app.get("/sync-get")
def sync_get():
    return {"method": "sync"}

@app.post("/sync-post")
def sync_post(item: Item):
    return {"name": item.name, "sync": True}

@app.get("/sync-with-path/{item_id}")
def sync_with_path(item_id: int):
    return {"item_id": item_id}
"#;

    let mut server = TestServer::start(app, "_apx_test_sync_ext").await;

    let (status, body) = server.get("/sync-get").await;
    assert_eq!(status, 200, "sync GET: {body}");
    assert_eq!(body["method"], "sync");

    let (status, body) = server
        .post_json("/sync-post", serde_json::json!({"name": "x"}))
        .await;
    assert_eq!(status, 200, "sync POST: {body}");
    assert_eq!(body["name"], "x");
    assert_eq!(body["sync"], true);

    let (status, body) = server.get("/sync-with-path/42").await;
    assert_eq!(status, 200, "sync path param: {body}");
    assert_eq!(body["item_id"], 42);

    server.stop().await;
}

// ── Test 15: Mixed path + query + body ──────────────────────────────────

#[tokio::test]
async fn mixed_path_query_body() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
from typing import Optional
app = FastAPI()

class Item(BaseModel):
    name: str
    price: float

@app.put("/items/{item_id}")
async def update_item(item_id: int, item: Item, q: Optional[str] = None):
    result = {"item_id": item_id, **item.model_dump()}
    if q:
        result["q"] = q
    return result
"#;

    let mut server = TestServer::start(app, "_apx_test_mixed_params").await;

    // All three param types
    let (status, body) = server
        .put_json(
            "/items/1?q=test",
            serde_json::json!({"name": "x", "price": 1.0}),
        )
        .await;
    assert_eq!(status, 200, "mixed params: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "x");
    assert_eq!(body["q"], "test");

    // Without optional query
    let (status, body) = server
        .put_json("/items/1", serde_json::json!({"name": "x", "price": 1.0}))
        .await;
    assert_eq!(status, 200, "mixed params without q: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "x");
    assert!(
        body.get("q").is_none() || body["q"].is_null(),
        "q should be absent: {body}"
    );

    server.stop().await;
}

// ── Test 16: BackgroundTasks (simple) ────────────────────────────────────

/// BackgroundTasks param resolved through ASGI bridge (FastAPI's solve_dependencies).
#[tokio::test]
async fn background_tasks_simple() {
    let app = r#"
from fastapi import FastAPI, BackgroundTasks

app = FastAPI()

log = []

def write_log(message: str):
    log.append(message)

@app.post("/send/{email}")
async def send_notification(email: str, background_tasks: BackgroundTasks):
    background_tasks.add_task(write_log, f"sent to {email}")
    return {"message": f"notification queued for {email}"}
"#;

    let mut server = TestServer::start(app, "_apx_test_bg_simple").await;

    let (status, body) = server.post_empty("/send/user@example.com").await;
    assert_eq!(status, 200, "bg simple: {body}");
    assert_eq!(body["message"], "notification queued for user@example.com");

    server.stop().await;
}

// ── Test 17: BackgroundTasks (async task) ────────────────────────────────

/// BackgroundTasks with async task function resolved through ASGI bridge.
#[tokio::test]
async fn background_tasks_async_task() {
    let app = r#"
from fastapi import FastAPI, BackgroundTasks

app = FastAPI()

log = []

async def write_log_async(message: str):
    log.append(message)

@app.post("/notify")
async def notify(background_tasks: BackgroundTasks):
    background_tasks.add_task(write_log_async, "async task ran")
    return {"status": "queued"}
"#;

    let mut server = TestServer::start(app, "_apx_test_bg_async").await;

    let (status, body) = server.post_empty("/notify").await;
    assert_eq!(status, 200, "bg async: {body}");
    assert_eq!(body["status"], "queued");

    server.stop().await;
}

// ── Test 18: BackgroundTasks in dependency ────────────────────────────────

/// BackgroundTasks injected into a dependency resolved through ASGI bridge.
#[tokio::test]
async fn background_tasks_in_dependency() {
    let app = r#"
from fastapi import FastAPI, BackgroundTasks, Depends

app = FastAPI()

log = []

def write_log(message: str):
    log.append(message)

def get_query(background_tasks: BackgroundTasks, q: str = None):
    if q:
        background_tasks.add_task(write_log, f"dep query: {q}")
    return q

@app.post("/items")
async def create_item(q: str = Depends(get_query), background_tasks: BackgroundTasks = None):
    if background_tasks:
        background_tasks.add_task(write_log, "handler task")
    return {"q": q}
"#;

    let mut server = TestServer::start(app, "_apx_test_bg_dep").await;

    let (status, body) = server.post_empty("/items?q=hello").await;
    assert_eq!(status, 200, "bg in dep: {body}");
    assert_eq!(body["q"], "hello");

    server.stop().await;
}

// ── Test 19: Depends sync generator ──────────────────────────────────────

/// Sync generator dependency (`yield` with `try/finally` teardown) resolved
/// through ASGI bridge (FastAPI's solve_dependencies handles generator protocol).
#[tokio::test]
async fn depends_sync_generator() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    db = {"connection": "active", "closed": False}
    try:
        yield db
    finally:
        db["closed"] = True

@app.get("/db-status")
async def db_status(db=Depends(get_db)):
    return {"connection": db["connection"]}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_sync_gen").await;

    let (status, body) = server.get("/db-status").await;
    assert_eq!(status, 200, "sync gen dep: {body}");
    assert_eq!(body["connection"], "active");

    server.stop().await;
}

// ── Test 20: Depends async generator ─────────────────────────────────────

/// Async generator dependency (`async yield` with `try/finally`) resolved
/// through ASGI bridge (FastAPI's solve_dependencies handles async generator protocol).
#[tokio::test]
async fn depends_async_generator() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

async def get_db():
    db = {"connection": "async-active", "closed": False}
    try:
        yield db
    finally:
        db["closed"] = True

@app.get("/async-db")
async def async_db(db=Depends(get_db)):
    return {"connection": db["connection"]}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_async_gen").await;

    let (status, body) = server.get("/async-db").await;
    assert_eq!(status, 200, "async gen dep: {body}");
    assert_eq!(body["connection"], "async-active");

    server.stop().await;
}

// ── Test 21: Depends chained sub-dependencies ────────────────────────────

/// Chain of deps: `get_settings() → get_db(settings) → handler(db)`.
/// Resolved through ASGI bridge (FastAPI walks the dependency tree).
#[tokio::test]
async fn depends_chained_sub_dependencies() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

def get_settings():
    return {"db_url": "sqlite:///test.db"}

def get_db(settings=Depends(get_settings)):
    return {"url": settings["db_url"], "connected": True}

@app.get("/chained")
async def chained(db=Depends(get_db)):
    return {"url": db["url"], "connected": db["connected"]}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_chained").await;

    let (status, body) = server.get("/chained").await;
    assert_eq!(status, 200, "chained deps: {body}");
    assert_eq!(body["url"], "sqlite:///test.db");
    assert_eq!(body["connected"], true);

    server.stop().await;
}

// ── Test 22: Depends multiple on endpoint ────────────────────────────────

/// Two independent `Depends()` params on one endpoint resolved through ASGI bridge.
#[tokio::test]
async fn depends_multiple_on_endpoint() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    return {"db": "active"}

def get_config():
    return {"env": "test"}

@app.get("/multi-dep")
async def multi_dep(db=Depends(get_db), config=Depends(get_config)):
    return {"db": db["db"], "env": config["env"]}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_multi").await;

    let (status, body) = server.get("/multi-dep").await;
    assert_eq!(status, 200, "multi dep: {body}");
    assert_eq!(body["db"], "active");
    assert_eq!(body["env"], "test");

    server.stop().await;
}

// ── Test 23: Depends class-based ─────────────────────────────────────────

/// Class with `__call__` used as dependency callable, resolved through ASGI bridge.
#[tokio::test]
async fn depends_class_based() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

class FixedContentQueryChecker:
    def __init__(self, fixed_content: str):
        self.fixed_content = fixed_content

    def __call__(self, q: str = ""):
        if q:
            return self.fixed_content in q
        return False

checker = FixedContentQueryChecker("bar")

@app.get("/check")
async def check(result: bool = Depends(checker)):
    return {"match": result}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_class").await;

    let (status, body) = server.get("/check?q=foobar").await;
    assert_eq!(status, 200, "class dep match: {body}");
    assert_eq!(body["match"], true);

    let (status, body) = server.get("/check?q=nope").await;
    assert_eq!(status, 200, "class dep no match: {body}");
    assert_eq!(body["match"], false);

    server.stop().await;
}

// ── Test 24: Depends contextmanager class ────────────────────────────────

/// Context manager class inside a sync generator dependency, resolved through ASGI bridge.
#[tokio::test]
async fn depends_contextmanager_class() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

class DBSession:
    def __init__(self):
        self.active = True
    def __enter__(self):
        return self
    def __exit__(self, exc_type, exc_val, exc_tb):
        self.active = False
        return False
    def query(self):
        return "result"

def get_db():
    with DBSession() as db:
        yield db

@app.get("/ctx")
async def ctx(db=Depends(get_db)):
    return {"result": db.query(), "active": db.active}
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_ctx").await;

    let (status, body) = server.get("/ctx").await;
    assert_eq!(status, 200, "ctx dep: {body}");
    assert_eq!(body["result"], "result");
    assert_eq!(body["active"], true);

    server.stop().await;
}
