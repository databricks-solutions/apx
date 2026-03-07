//! Request body parsing: JSON, form data, file upload, and mixed parameter tests.

use super::TestServer;

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

    let (status, body) = server
        .post_json("/users", serde_json::json!({"name": "a", "address": {}}))
        .await;
    assert_eq!(status, 422, "invalid nested should be 422: {body}");

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

    let (status, body) = server
        .post_json(
            "/items",
            serde_json::json!({"name": "widget", "price": 9.99}),
        )
        .await;
    assert_eq!(status, 200, "valid body failed: {body}");
    assert_eq!(body["name"], "widget");

    // Missing required field → 422
    let (status, body) = server
        .post_json("/items", serde_json::json!({"name": "widget"}))
        .await;
    assert_eq!(status, 422, "missing field should be 422: {body}");
    assert!(
        body["detail"].is_array(),
        "422 should have detail array: {body}"
    );

    // Invalid JSON → 422
    let (status, _body) = server
        .post_raw("/items", "not json", "application/json")
        .await;
    assert!(
        status == 400 || status == 422,
        "invalid JSON should be 400 or 422, got {status}"
    );

    server.stop().await;
}

/// Combined path + query + body on same endpoint.
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

/// Multiple body params: FastAPI wraps in `{"item": {...}, "user": {...}}`.
#[tokio::test]
async fn multiple_body_params() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel

app = FastAPI()

class Item(BaseModel):
    name: str

class User(BaseModel):
    username: str

@app.post("/create")
async def create(item: Item, user: User):
    return {"item_name": item.name, "username": user.username}
"#;

    let mut server = TestServer::start(app, "_apx_test_multi_body").await;

    let (status, body) = server
        .post_json(
            "/create",
            serde_json::json!({"item": {"name": "widget"}, "user": {"username": "alice"}}),
        )
        .await;
    assert_eq!(status, 200, "multiple body params: {body}");
    assert_eq!(body["item_name"], "widget");
    assert_eq!(body["username"], "alice");

    // Flat body (missing wrapping) → 422
    let (status, _body) = server
        .post_json(
            "/create",
            serde_json::json!({"name": "widget", "username": "alice"}),
        )
        .await;
    assert_eq!(status, 422, "flat body should be 422 for multi-body");

    server.stop().await;
}

/// `Body(embed=True)` forces single body param to be wrapped.
#[tokio::test]
async fn body_embed() {
    let app = r#"
from fastapi import FastAPI, Body
from pydantic import BaseModel

app = FastAPI()

class Item(BaseModel):
    name: str

@app.post("/embedded")
async def embedded(item: Item = Body(embed=True)):
    return {"name": item.name}
"#;

    let mut server = TestServer::start(app, "_apx_test_body_embed").await;

    let (status, body) = server
        .post_json("/embedded", serde_json::json!({"item": {"name": "widget"}}))
        .await;
    assert_eq!(status, 200, "body embed: {body}");
    assert_eq!(body["name"], "widget");

    // Flat body → 422
    let (status, _body) = server
        .post_json("/embedded", serde_json::json!({"name": "widget"}))
        .await;
    assert_eq!(status, 422, "flat body should be 422 with embed=True");

    server.stop().await;
}

/// `Form(...)` with `application/x-www-form-urlencoded`.
#[tokio::test]
async fn form_data() {
    let app = r#"
from fastapi import FastAPI, Form

app = FastAPI()

@app.post("/login")
async def login(username: str = Form(), password: str = Form()):
    return {"username": username, "password_len": len(password)}
"#;

    let mut server = TestServer::start(app, "_apx_test_form_data").await;

    let (status, body) = server
        .post_form(
            "/login",
            &[("username", "alice"), ("password", "secret123")],
        )
        .await;
    assert_eq!(status, 200, "form data: {body}");
    assert_eq!(body["username"], "alice");
    assert_eq!(body["password_len"], 9);

    let (status, _body) = server.post_form("/login", &[("username", "alice")]).await;
    assert_eq!(status, 422, "missing form field should be 422");

    server.stop().await;
}

/// `UploadFile = File(...)` with `multipart/form-data`.
#[tokio::test]
async fn file_upload() {
    let app = r#"
from fastapi import FastAPI, File, UploadFile

app = FastAPI()

@app.post("/upload")
async def upload(file: UploadFile = File()):
    content = await file.read()
    return {
        "filename": file.filename,
        "size": len(content),
        "content": content.decode("utf-8"),
    }
"#;

    let mut server = TestServer::start(app, "_apx_test_file_upload").await;

    let file_content = b"hello file upload".to_vec();
    let (status, body) = server
        .post_multipart("/upload", "file", "test.txt", file_content)
        .await;
    assert_eq!(status, 200, "file upload: {body}");
    assert_eq!(body["filename"], "test.txt");
    assert_eq!(body["size"], 17);
    assert_eq!(body["content"], "hello file upload");

    server.stop().await;
}

/// Sending `text/plain` content-type to an endpoint expecting JSON body → 422.
#[tokio::test]
async fn wrong_content_type_for_json() {
    let app = r#"
from fastapi import FastAPI
from pydantic import BaseModel
app = FastAPI()

class Item(BaseModel):
    name: str

@app.post("/items")
async def create_item(item: Item):
    return {"name": item.name}
"#;

    let mut server = TestServer::start(app, "_apx_test_wrong_ct").await;

    let (status, _body) = server
        .post_raw("/items", r#"{"name": "x"}"#, "text/plain")
        .await;
    assert_eq!(
        status, 422,
        "text/plain content-type for JSON endpoint should be 422"
    );

    server.stop().await;
}

/// POST `{}` to a handler with no body params → 200 (body is ignored).
#[tokio::test]
async fn empty_json_to_no_body_handler() {
    let app = r#"
from fastapi import FastAPI
app = FastAPI()

@app.post("/empty-ok")
async def empty_ok():
    return {"ok": True}
"#;

    let mut server = TestServer::start(app, "_apx_test_empty_json").await;

    let (status, body) = server.post_json("/empty-ok", serde_json::json!({})).await;
    assert_eq!(status, 200, "empty JSON to no-body handler: {body}");
    assert_eq!(body["ok"], true);

    server.stop().await;
}
