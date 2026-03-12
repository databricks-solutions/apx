//! Integration tests for manifest-based serving.
//!
//! Tests the full roundtrip: discover → save manifest → load manifest → bind → serve.

use super::TestServer;

// ── Basic roundtrip ─────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_basic() {
    let app = r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/hello")
async def hello():
    return {"message": "hello"}

@app.post("/echo")
async def echo(data: dict):
    return data
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_basic").await;

    let (status, body) = server.get("/hello").await;
    assert_eq!(status, 200, "GET /hello failed: {body}");
    assert_eq!(body["message"], "hello");

    let (status, body) = server
        .post_json("/echo", serde_json::json!({"key": "value"}))
        .await;
    assert_eq!(status, 200, "POST /echo failed: {body}");
    assert_eq!(body["key"], "value");

    server.stop().await;
}

// ── Dependencies ────────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_with_deps() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    return {"db": "connected"}

@app.get("/data")
async def data(db = Depends(get_db)):
    return db
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_deps").await;

    let (status, body) = server.get("/data").await;
    assert_eq!(status, 200, "GET /data failed: {body}");
    assert_eq!(body["db"], "connected");

    server.stop().await;
}

// ── Path parameters ─────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_path_params() {
    let app = r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def get_item(item_id: int):
    return {"item_id": item_id}
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_path").await;

    let (status, body) = server.get("/items/42").await;
    assert_eq!(status, 200, "GET /items/42 failed: {body}");
    assert_eq!(body["item_id"], 42);

    server.stop().await;
}

// ── Query parameters ────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_query_params() {
    let app = r#"
from fastapi import FastAPI
from typing import Optional

app = FastAPI()

@app.get("/search")
async def search(q: str, page: int = 1):
    return {"q": q, "page": page}
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_query").await;

    let (status, body) = server.get("/search?q=test&page=2").await;
    assert_eq!(status, 200, "GET /search failed: {body}");
    assert_eq!(body["q"], "test");
    assert_eq!(body["page"], 2);

    let (status, body) = server.get("/search?q=default").await;
    assert_eq!(status, 200, "GET /search default page failed: {body}");
    assert_eq!(body["page"], 1);

    server.stop().await;
}

// ── Sync handlers ───────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_sync_handlers() {
    let app = r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/sync")
def sync_handler():
    return {"mode": "sync"}
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_sync").await;

    let (status, body) = server.get("/sync").await;
    assert_eq!(status, 200, "GET /sync failed: {body}");
    assert_eq!(body["mode"], "sync");

    server.stop().await;
}

// ── Multiple HTTP methods ───────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_multiple_methods() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()

class Item(BaseModel):
    name: str

@app.put("/items/{item_id}")
async def replace_item(item_id: int, item: Item):
    return {"item_id": item_id, "name": item.name}

@app.patch("/items/{item_id}")
async def update_item(item_id: int, item: Item):
    return {"item_id": item_id, "name": item.name, "partial": True}

@app.delete("/items/{item_id}")
async def delete_item(item_id: int):
    return {"deleted": item_id}
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_methods").await;

    let (status, body) = server
        .put_json("/items/1", serde_json::json!({"name": "x"}))
        .await;
    assert_eq!(status, 200, "PUT: {body}");
    assert_eq!(body["item_id"], 1);

    let (status, body) = server
        .patch_json("/items/2", serde_json::json!({"name": "y"}))
        .await;
    assert_eq!(status, 200, "PATCH: {body}");
    assert_eq!(body["partial"], true);

    let (status, body) = server.delete("/items/3").await;
    assert_eq!(status, 200, "DELETE: {body}");
    assert_eq!(body["deleted"], 3);

    server.stop().await;
}

// ── Body validation ─────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip_body_validation() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()

class CreateUser(BaseModel):
    name: str
    age: int

@app.post("/users")
async def create_user(user: CreateUser):
    return {"name": user.name, "age": user.age}
"#;

    let mut server = TestServer::start_from_manifest(app, "_apx_test_manifest_body").await;

    // Valid body.
    let (status, body) = server
        .post_json("/users", serde_json::json!({"name": "Alice", "age": 30}))
        .await;
    assert_eq!(status, 200, "valid body: {body}");
    assert_eq!(body["name"], "Alice");
    assert_eq!(body["age"], 30);

    // Invalid body — missing required field.
    let (status, _body) = server
        .post_json("/users", serde_json::json!({"name": "Bob"}))
        .await;
    assert_eq!(status, 422, "missing field should return 422");

    server.stop().await;
}

// ── Manifest validation (unit-style, no server) ────────────────────────

#[test]
fn manifest_missing_meta_rejected() {
    let manifest = crate::route::AppManifest {
        meta: None,
        routes: Vec::new(),
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: crate::route::BodyLimit::DEFAULT,
        validation_results: Vec::new(),
        has_middleware: false,
    };
    let err = crate::manifest::validate_for_serving(&manifest).unwrap_err();
    assert!(
        matches!(err, crate::manifest::ManifestError::MissingMeta),
        "expected MissingMeta, got: {err}"
    );
}

#[test]
fn manifest_version_check() {
    let manifest = crate::route::AppManifest {
        meta: Some(crate::route::ManifestMeta {
            apx_version: "0.0.0-wrong".to_owned(),
            python_version: "3.12.0".to_owned(),
            fastapi_version: None,
            build_timestamp: "2025-01-01T00:00:00Z".to_owned(),
            app_module: crate::route::AppModule::new("backend.app").unwrap(),
            source_hash: None,
        }),
        routes: Vec::new(),
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: crate::route::BodyLimit::DEFAULT,
        validation_results: Vec::new(),
        has_middleware: false,
    };
    let err = crate::manifest::validate_for_serving(&manifest).unwrap_err();
    assert!(
        matches!(err, crate::manifest::ManifestError::VersionMismatch { .. }),
        "expected VersionMismatch, got: {err}"
    );
}

#[test]
fn manifest_serde_preserves_dependency_plan() {
    use crate::route::*;

    let plan = DependencyPlan {
        steps: vec![
            DependencyStep::ExtractQuery {
                name: "page".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
                required: true,
                default_json: None,
            },
            DependencyStep::CallPython {
                dep_qualname: QualName::new("deps.get_db").unwrap(),
                target_kwarg: "db".to_owned(),
                inputs: vec!["page".to_owned()],
                is_generator: false,
                is_async: false,
                use_cache: true,
            },
        ],
        handler_kwargs: vec!["db".to_owned()],
        needs_asgi: false,
        generator_cleanup_indices: vec![],
    };

    let route = RouteManifest {
        kind: HandlerKind::RequestResponse,
        method: HttpMethod::Get,
        path: RoutePath::new("/test").unwrap(),
        handler_qualname: QualName::new("mod.handler").unwrap(),
        params: Vec::new(),
        response_type: ResponseType::RawResponse,
        tags: Vec::new(),
        dependency_plan: Some(plan),
        status_code: 200,
        summary: None,
        description: None,
        include_in_schema: true,
        deprecated: false,
        operation_id: None,
        is_async_handler: true,
        dispatch_strategy: DispatchStrategy::default(),
    };

    let manifest = AppManifest {
        meta: Some(ManifestMeta {
            apx_version: env!("CARGO_PKG_VERSION").to_owned(),
            python_version: "3.12.0".to_owned(),
            fastapi_version: None,
            build_timestamp: "2025-01-01T00:00:00Z".to_owned(),
            app_module: AppModule::new("backend.app").unwrap(),
            source_hash: None,
        }),
        routes: vec![route],
        dependency_graph: Vec::new(),
        lifecycle_deps: Vec::new(),
        openapi_schema: None,
        max_body_limit: BodyLimit::DEFAULT,
        validation_results: Vec::new(),
        has_middleware: false,
    };

    let json = serde_json::to_string(&manifest).unwrap();
    let loaded: AppManifest = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.routes.len(), 1);
    let loaded_plan = loaded.routes[0].dependency_plan.as_ref().unwrap();
    assert_eq!(loaded_plan.steps.len(), 2);
    assert_eq!(loaded_plan.handler_kwargs, vec!["db"]);
}
