//! Path, query, header, cookie, and enum parameter extraction tests.

use super::TestServer;

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

    let (status, body) = server.get("/items/42").await;
    assert_eq!(status, 200, "path param int: {body}");
    assert_eq!(body["item_id"], 42);

    let (status, _body) = server.get("/items/abc").await;
    assert_eq!(status, 422, "invalid int path param should be 422");

    let (status, body) = server.get("/search?q=hello").await;
    assert_eq!(status, 200, "query present: {body}");
    assert_eq!(body["q"], "hello");

    let (status, _body) = server.get("/search").await;
    assert_eq!(status, 422, "missing required query should be 422");

    let (status, body) = server
        .get_with_headers("/with-header", &[("x-token", "secret")])
        .await;
    assert_eq!(status, 200, "header param: {body}");
    assert_eq!(body["token"], "secret");

    let (status, _body) = server.get("/with-header").await;
    assert_eq!(status, 422, "missing header should be 422");

    server.stop().await;
}

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

    let (status, body) = server.get("/items/42").await;
    assert_eq!(status, 200, "GET /items/42: {body}");
    assert_eq!(body["item_id"], 42);

    let (status, body) = server.get("/items/0").await;
    assert_eq!(status, 200, "GET /items/0: {body}");
    assert_eq!(body["item_id"], 0);

    let (status, body) = server.get("/items/-1").await;
    assert_eq!(status, 200, "GET /items/-1: {body}");
    assert_eq!(body["item_id"], -1);

    let (status, _body) = server.get("/items/abc").await;
    assert_eq!(status, 422, "non-int path param should be 422");

    let (status, _body) = server.get("/items/3.14").await;
    assert_eq!(status, 422, "float path param should be 422 for int route");

    let (status, body) = server.get("/users/alice").await;
    assert_eq!(status, 200, "GET /users/alice: {body}");
    assert_eq!(body["username"], "alice");

    let (status, body) = server.get("/users/hello%20world").await;
    assert_eq!(status, 200, "URL-decoded path param: {body}");
    assert_eq!(body["username"], "hello world");

    let (status, body) = server.get("/floats/3.14").await;
    assert_eq!(status, 200, "GET /floats/3.14: {body}");
    let value = body["value"].as_f64().unwrap();
    #[expect(clippy::approx_constant, reason = "testing exact float parsing")]
    let expected = 3.14_f64;
    assert!((value - expected).abs() < 0.001);

    server.stop().await;
}

/// `:path` catch-all route param is not supported by the axum/matchit router.
#[tokio::test]
#[should_panic(expected = "path param catch-all unsupported")]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "Python f-string in raw string literal"
)]
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

    let (status, body) = server.get("/required?q=hello").await;
    assert_eq!(status, 200, "required query: {body}");
    assert_eq!(body["q"], "hello");

    let (status, _body) = server.get("/required").await;
    assert_eq!(status, 422, "missing required query should be 422");

    let (status, body) = server.get("/required?q=").await;
    assert_eq!(status, 200, "empty string query: {body}");
    assert_eq!(body["q"], "");

    let (status, body) = server.get("/required?q=hello%20world").await;
    assert_eq!(status, 200, "URL-decoded query: {body}");
    assert_eq!(body["q"], "hello world");

    let (status, body) = server.get("/optional").await;
    assert_eq!(status, 200, "optional absent: {body}");
    assert!(
        body["q"].is_null(),
        "optional absent should be null: {body}"
    );

    let (status, body) = server.get("/optional?q=test").await;
    assert_eq!(status, 200, "optional present: {body}");
    assert_eq!(body["q"], "test");

    server.stop().await;
}

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

    let (status, body) = server.get("/with-default?page=5").await;
    assert_eq!(status, 200, "partial override: {body}");
    assert_eq!(body["page"], 5, "page override: {body}");
    assert_eq!(body["size"], 10, "size still default: {body}");

    server.stop().await;
}

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

    let (status, body) = server
        .get_with_headers("/with-header", &[("x-token", "abc")])
        .await;
    assert_eq!(status, 200, "required header: {body}");
    assert_eq!(body["token"], "abc");

    let (status, _body) = server.get("/with-header").await;
    assert_eq!(status, 422, "missing required header should be 422");

    let (status, body) = server.get("/optional-header").await;
    assert_eq!(status, 200, "optional header absent: {body}");
    assert!(
        body["debug"].is_null(),
        "optional header should be null: {body}"
    );

    let (status, body) = server
        .get_with_headers("/optional-header", &[("x-debug", "on")])
        .await;
    assert_eq!(status, 200, "optional header present: {body}");
    assert_eq!(body["debug"], "on");

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

    let (status, body) = server
        .get_with_headers("/with-cookie", &[("cookie", "session_id=abc123")])
        .await;
    assert_eq!(status, 200, "cookie present: {body}");
    assert_eq!(body["session"], "abc123");

    let (status, _body) = server.get("/with-cookie").await;
    assert_eq!(status, 422, "missing required cookie should be 422");

    let (status, body) = server.get("/optional-cookie").await;
    assert_eq!(status, 200, "optional cookie absent: {body}");
    assert!(
        body["tracking"].is_null(),
        "optional cookie should be null: {body}"
    );

    server.stop().await;
}

/// `class Color(str, Enum)` as path and query param — FastAPI validates against enum values.
#[tokio::test]
async fn enum_params() {
    let app = r#"
from enum import Enum
from fastapi import FastAPI

app = FastAPI()

class Color(str, Enum):
    red = "red"
    green = "green"
    blue = "blue"

@app.get("/colors/{color}")
async def get_color(color: Color):
    return {"color": color.value}

@app.get("/filter")
async def filter_by_color(color: Color):
    return {"color": color.value}
"#;

    let mut server = TestServer::start(app, "_apx_test_enum_params").await;

    let (status, body) = server.get("/colors/red").await;
    assert_eq!(status, 200, "enum path valid: {body}");
    assert_eq!(body["color"], "red");

    let (status, _body) = server.get("/colors/yellow").await;
    assert_eq!(status, 422, "invalid enum path should be 422");

    let (status, body) = server.get("/filter?color=green").await;
    assert_eq!(status, 200, "enum query valid: {body}");
    assert_eq!(body["color"], "green");

    let (status, _body) = server.get("/filter?color=purple").await;
    assert_eq!(status, 422, "invalid enum query should be 422");

    server.stop().await;
}

/// `Query(min_length=...)`, `Path(ge=...)` — FastAPI validates via Pydantic.
#[tokio::test]
async fn validation_constraints() {
    let app = r#"
from fastapi import FastAPI, Query, Path

app = FastAPI()

@app.get("/search")
async def search(q: str = Query(min_length=3, max_length=50)):
    return {"q": q}

@app.get("/items/{item_id}")
async def get_item(item_id: int = Path(ge=1)):
    return {"item_id": item_id}
"#;

    let mut server = TestServer::start(app, "_apx_test_validation_constraints").await;

    let (status, body) = server.get("/search?q=hello").await;
    assert_eq!(status, 200, "valid query: {body}");
    assert_eq!(body["q"], "hello");

    let (status, _body) = server.get("/search?q=ab").await;
    assert_eq!(status, 422, "too short query should be 422");

    let (status, body) = server.get("/items/1").await;
    assert_eq!(status, 200, "valid path ge=1: {body}");
    assert_eq!(body["item_id"], 1);

    let (status, _body) = server.get("/items/0").await;
    assert_eq!(status, 422, "zero should be 422 with ge=1");

    let (status, _body) = server.get("/items/-1").await;
    assert_eq!(status, 422, "negative should be 422 with ge=1");

    server.stop().await;
}
