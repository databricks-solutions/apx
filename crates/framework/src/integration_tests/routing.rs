//! Routing edge cases: 404, 405, health probes, trailing slash, multiple routers.

use super::TestServer;

/// GET to a path that doesn't exist → 404.
#[tokio::test]
async fn routing_404_nonexistent_path() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}
"#;

    let mut server = TestServer::start(app, "_apx_test_404_nonexistent").await;

    let (status, _body) = server.get("/does-not-exist").await;
    assert_eq!(status, 404, "non-existent path should be 404");

    server.stop().await;
}

/// POST to a GET-only endpoint → 405 Method Not Allowed.
#[tokio::test]
async fn routing_405_wrong_method() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}
"#;

    let mut server = TestServer::start(app, "_apx_test_405_method").await;

    let (status, _body) = server.post_empty("/hello").await;
    assert_eq!(status, 405, "POST on GET-only route should be 405");

    server.stop().await;
}

/// Built-in `/healthz` probe returns 200 with `{"status": "alive"}`.
#[tokio::test]
async fn healthz_probe() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}
"#;

    let mut server = TestServer::start(app, "_apx_test_healthz").await;

    let (status, body) = server.get("/healthz").await;
    assert_eq!(status, 200, "healthz probe: {body}");
    assert_eq!(body["status"], "alive");

    server.stop().await;
}

/// Built-in `/readyz` probe returns 200 with `{"status": "ready"}`.
#[tokio::test]
async fn readyz_probe() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}
"#;

    let mut server = TestServer::start(app, "_apx_test_readyz").await;

    let (status, body) = server.get("/readyz").await;
    assert_eq!(status, 200, "readyz probe: {body}");
    assert_eq!(body["status"], "ready");

    server.stop().await;
}

/// User-defined `/healthz` route takes precedence over the built-in probe.
#[tokio::test]
async fn healthz_user_override() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/healthz")
async def custom_healthz():
    return {"status": "custom", "db": "ok"}
"#;

    let mut server = TestServer::start(app, "_apx_test_healthz_override").await;

    let (status, body) = server.get("/healthz").await;
    assert_eq!(status, 200, "user healthz: {body}");
    assert_eq!(body["status"], "custom", "user handler should win");
    assert_eq!(body["db"], "ok");

    server.stop().await;
}

/// GET `/items/` when only `/items` is registered → 404 (axum strict trailing slash).
#[tokio::test]
async fn trailing_slash_no_match() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.get("/items")
async def list_items():
    return {"items": []}
"#;

    let mut server = TestServer::start(app, "_apx_test_trailing_slash").await;

    // Exact path works
    let (status, body) = server.get("/items").await;
    assert_eq!(status, 200, "exact path: {body}");

    // Trailing slash → 404
    let (status, _body) = server.get("/items/").await;
    assert_eq!(status, 404, "trailing slash should be 404");

    server.stop().await;
}

/// Two `APIRouter(prefix=...)` instances on the same app — both work, no collisions.
#[tokio::test]
async fn multiple_routers_with_prefix() {
    let app = r#"
from fastapi import FastAPI, APIRouter

app = FastAPI()

users_router = APIRouter(prefix="/api/users")

@users_router.get("")
async def list_users():
    return {"users": ["alice", "bob"]}

@users_router.get("/{user_id}")
async def get_user(user_id: int):
    return {"user_id": user_id, "source": "users"}

products_router = APIRouter(prefix="/api/products")

@products_router.get("")
async def list_products():
    return {"products": ["widget", "gadget"]}

@products_router.get("/{product_id}")
async def get_product(product_id: int):
    return {"product_id": product_id, "source": "products"}

app.include_router(users_router)
app.include_router(products_router)
"#;

    let mut server = TestServer::start(app, "_apx_test_multi_router").await;

    // Users router
    let (status, body) = server.get("/api/users").await;
    assert_eq!(status, 200, "GET /api/users: {body}");
    assert_eq!(body["users"], serde_json::json!(["alice", "bob"]));

    let (status, body) = server.get("/api/users/1").await;
    assert_eq!(status, 200, "GET /api/users/1: {body}");
    assert_eq!(body["user_id"], 1);
    assert_eq!(body["source"], "users");

    // Products router
    let (status, body) = server.get("/api/products").await;
    assert_eq!(status, 200, "GET /api/products: {body}");
    assert_eq!(body["products"], serde_json::json!(["widget", "gadget"]));

    let (status, body) = server.get("/api/products/42").await;
    assert_eq!(status, 200, "GET /api/products/42: {body}");
    assert_eq!(body["product_id"], 42);
    assert_eq!(body["source"], "products");

    // Cross-prefix routes don't collide — users path on products prefix → 404
    let (status, _body) = server.get("/api/products/users").await;
    assert_eq!(status, 422, "cross-prefix should not match string as int");

    server.stop().await;
}
