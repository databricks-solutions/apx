//! Basic handler dispatch, HTTP methods, sync handlers, and routing tests.

use super::TestServer;

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

    let (status, body) = server
        .put_json("/items/1", serde_json::json!({"name": "x", "price": 1.0}))
        .await;
    assert_eq!(status, 200, "PUT: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "x");

    let (status, body) = server
        .patch_json("/items/1", serde_json::json!({"name": "y", "price": 2.0}))
        .await;
    assert_eq!(status, 200, "PATCH: {body}");
    assert_eq!(body["item_id"], 1);
    assert_eq!(body["name"], "y");

    let (status, body) = server.delete("/items/1").await;
    assert_eq!(status, 200, "DELETE: {body}");
    assert_eq!(body["deleted"], 1);

    server.stop().await;
}

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

/// `APIRouter(prefix=...)` + `app.include_router(router)` — discovery walks
/// `app.routes` which flattens included routers into the top-level route table.
#[tokio::test]
async fn include_router_prefix() {
    let app = r#"
from fastapi import FastAPI, APIRouter

router = APIRouter(prefix="/api/v1")

@router.get("/items")
async def list_items():
    return {"items": ["a", "b"]}

@router.get("/items/{item_id}")
async def get_item(item_id: int):
    return {"item_id": item_id}

@router.post("/items")
async def create_item():
    return {"created": True}

app = FastAPI()
app.include_router(router)
"#;

    let mut server = TestServer::start(app, "_apx_test_include_router").await;

    let (status, body) = server.get("/api/v1/items").await;
    assert_eq!(status, 200, "GET /api/v1/items: {body}");
    assert_eq!(body["items"], serde_json::json!(["a", "b"]));

    let (status, body) = server.get("/api/v1/items/42").await;
    assert_eq!(status, 200, "GET /api/v1/items/42: {body}");
    assert_eq!(body["item_id"], 42);

    let (status, body) = server.post_empty("/api/v1/items").await;
    assert_eq!(status, 200, "POST /api/v1/items: {body}");
    assert_eq!(body["created"], true);

    // Non-prefixed path should 404/405
    let (status, _body) = server.get("/items").await;
    assert!(
        status == 404 || status == 405,
        "non-prefixed /items should fail, got {status}"
    );

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
