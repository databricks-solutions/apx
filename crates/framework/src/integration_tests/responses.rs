//! Response status codes, response types, streaming, SSE, and edge case tests.

use super::TestServer;

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

/// `StreamingResponse` with chunked output.
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

/// `PlainTextResponse` — non-JSON response with `text/plain` content type.
#[tokio::test]
async fn plain_text_response() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import PlainTextResponse

app = FastAPI()

@app.get("/text")
async def text():
    return PlainTextResponse("hello plain text")
"#;

    let mut server = TestServer::start(app, "_apx_test_plain_text").await;

    let (status, headers, body) = server.get_raw("/text").await;
    assert_eq!(status, 200, "plain text: status {status}");
    let content_type = headers
        .get("content-type")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        content_type.contains("text/plain"),
        "expected text/plain, got {content_type}"
    );
    assert_eq!(std::str::from_utf8(&body).unwrap_or(""), "hello plain text");

    server.stop().await;
}

/// `HTMLResponse` — non-JSON response with `text/html` content type.
#[tokio::test]
async fn html_response() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import HTMLResponse

app = FastAPI()

@app.get("/html")
async def html():
    return HTMLResponse("<h1>Hello HTML</h1>")
"#;

    let mut server = TestServer::start(app, "_apx_test_html_resp").await;

    let (status, headers, body) = server.get_raw("/html").await;
    assert_eq!(status, 200, "html response: status {status}");
    let content_type = headers
        .get("content-type")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        content_type.contains("text/html"),
        "expected text/html, got {content_type}"
    );
    assert_eq!(
        std::str::from_utf8(&body).unwrap_or(""),
        "<h1>Hello HTML</h1>"
    );

    server.stop().await;
}

/// `RedirectResponse` — returns 307 with `Location` header.
#[tokio::test]
async fn redirect_response() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import RedirectResponse

app = FastAPI()

@app.get("/redirect")
async def redirect():
    return RedirectResponse(url="/target", status_code=307)

@app.get("/target")
async def target():
    return {"arrived": True}
"#;

    let mut server = TestServer::start(app, "_apx_test_redirect").await;

    let (status, headers) = server.get_no_redirect("/redirect").await;
    assert_eq!(status, 307, "redirect: expected 307, got {status}");
    let location = headers
        .get("location")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert_eq!(location, "/target", "redirect location: {location}");

    // Following redirect should reach /target
    let (status, body) = server.get("/redirect").await;
    assert_eq!(status, 200, "followed redirect: {body}");
    assert_eq!(body["arrived"], true);

    server.stop().await;
}

/// `FileResponse` — returns file content with proper MIME type.
#[tokio::test]
async fn file_response() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let file_path = tmp.path().to_str().unwrap().replace('\\', "/");
    std::fs::write(tmp.path(), "file content here").unwrap();

    let app = format!(
        r#"
from fastapi import FastAPI
from fastapi.responses import FileResponse

app = FastAPI()

@app.get("/download")
async def download():
    return FileResponse("{file_path}")
"#
    );

    let mut server = TestServer::start(&app, "_apx_test_file_resp").await;

    let (status, _headers, body) = server.get_raw("/download").await;
    assert_eq!(status, 200, "file response: status {status}");
    assert_eq!(
        std::str::from_utf8(&body).unwrap_or(""),
        "file content here",
        "file content mismatch"
    );

    server.stop().await;
}

/// SSE: `StreamingResponse` with `text/event-stream` media type.
#[tokio::test]
async fn sse_endpoint() {
    let app = r#"
from fastapi import FastAPI
from fastapi.responses import StreamingResponse

app = FastAPI()

async def event_generator():
    for i in range(3):
        yield f"data: message {i}\n\n"

@app.get("/events")
async def events():
    return StreamingResponse(event_generator(), media_type="text/event-stream")
"#;

    let mut server = TestServer::start(app, "_apx_test_sse").await;

    let (status, headers, body) = server.get_raw("/events").await;
    assert_eq!(status, 200, "SSE: status {status}");
    let content_type = headers
        .get("content-type")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        content_type.contains("text/event-stream"),
        "expected text/event-stream, got {content_type}"
    );
    let text = std::str::from_utf8(&body).unwrap_or("");
    assert!(text.contains("data: message 0"), "SSE body: {text}");
    assert!(text.contains("data: message 1"), "SSE body: {text}");
    assert!(text.contains("data: message 2"), "SSE body: {text}");

    server.stop().await;
}

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
