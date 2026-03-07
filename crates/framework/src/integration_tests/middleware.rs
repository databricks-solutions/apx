//! Error handling, middleware, lifespan, and request/response injection tests.

use super::TestServer;

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

/// ValueError/TypeError/KeyError → 500, no internal detail leak.
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

    let (status, body) = server.get("/value-error").await;
    assert_eq!(status, 500, "ValueError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("bad value"),
        "ValueError detail leaked: {body}"
    );

    let (status, body) = server.get("/type-error").await;
    assert_eq!(status, 500, "TypeError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("wrong type"),
        "TypeError detail leaked: {body}"
    );

    let (status, body) = server.get("/key-error").await;
    assert_eq!(status, 500, "KeyError: {body}");
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains("missing"),
        "KeyError detail leaked: {body}"
    );

    server.stop().await;
}

/// Custom exception handler: `@app.exception_handler(ValueError)`.
#[tokio::test]
async fn custom_exception_handler() {
    let app = r#"
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

app = FastAPI()

@app.exception_handler(ValueError)
async def value_error_handler(request: Request, exc: ValueError):
    return JSONResponse(
        status_code=418,
        content={"error": "value_error", "message": str(exc)},
    )

@app.get("/raise-value-error")
async def raise_value_error():
    raise ValueError("custom handled")
"#;

    let mut server = TestServer::start(app, "_apx_test_exc_handler").await;

    let (status, body) = server.get("/raise-value-error").await;
    assert_eq!(status, 418, "custom exception handler: {body}");
    assert_eq!(body["error"], "value_error");
    assert_eq!(body["message"], "custom handled");

    server.stop().await;
}

/// Starlette `BaseHTTPMiddleware` subclass adding custom headers.
#[tokio::test]
async fn starlette_middleware() {
    let app = r#"
from fastapi import FastAPI
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request

app = FastAPI()

class TimingMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        response = await call_next(request)
        response.headers["x-middleware"] = "applied"
        return response

app.add_middleware(TimingMiddleware)

@app.get("/mw-test")
async def mw_test():
    return {"ok": True}
"#;

    let mut server = TestServer::start(app, "_apx_test_middleware").await;

    let (status, headers, body) = server.get_raw("/mw-test").await;
    assert_eq!(status, 200, "middleware: status {status}");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    assert_eq!(json["ok"], true);
    assert_eq!(
        headers
            .get("x-middleware")
            .map(|v| v.to_str().unwrap_or("")),
        Some("applied"),
        "x-middleware header missing: {headers:?}"
    );

    server.stop().await;
}

/// `FastAPI(lifespan=...)` context manager sets state accessible via `request.app.state`.
#[tokio::test]
async fn lifespan_state() {
    let app = r#"
from contextlib import asynccontextmanager
from fastapi import FastAPI, Request

@asynccontextmanager
async def lifespan(app):
    app.state.db_url = "sqlite:///test.db"
    app.state.started = True
    yield
    app.state.started = False

app = FastAPI(lifespan=lifespan)

@app.get("/state")
async def read_state(request: Request):
    return {
        "db_url": request.app.state.db_url,
        "started": request.app.state.started,
    }
"#;

    let mut server = TestServer::start(app, "_apx_test_lifespan").await;

    let (status, body) = server.get("/state").await;
    assert_eq!(status, 200, "lifespan state: {body}");
    assert_eq!(body["db_url"], "sqlite:///test.db");
    assert_eq!(body["started"], true);

    server.stop().await;
}

/// `request: Request` injection — accessing raw request data.
#[tokio::test]
async fn request_injection() {
    let app = r#"
from fastapi import FastAPI, Request

app = FastAPI()

@app.get("/req-info")
async def req_info(request: Request):
    return {
        "method": request.method,
        "path": request.url.path,
        "host": request.headers.get("host", ""),
    }
"#;

    let mut server = TestServer::start(app, "_apx_test_req_inject").await;

    let (status, body) = server.get("/req-info").await;
    assert_eq!(status, 200, "request injection: {body}");
    assert_eq!(body["method"], "GET");
    assert_eq!(body["path"], "/req-info");
    assert!(
        !body["host"].as_str().unwrap_or("").is_empty(),
        "host header should be present: {body}"
    );

    server.stop().await;
}

/// `response: Response` injection — setting custom headers from handler.
#[tokio::test]
async fn response_injection() {
    let app = r#"
from fastapi import FastAPI, Response

app = FastAPI()

@app.get("/with-custom-header")
async def with_custom_header(response: Response):
    response.headers["x-custom"] = "hello"
    response.headers["x-request-id"] = "abc-123"
    return {"ok": True}
"#;

    let mut server = TestServer::start(app, "_apx_test_resp_inject").await;

    let (status, headers, body) = server.get_raw("/with-custom-header").await;
    assert_eq!(status, 200, "response injection: status {status}");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    assert_eq!(json["ok"], true);
    assert_eq!(
        headers.get("x-custom").map(|v| v.to_str().unwrap_or("")),
        Some("hello"),
        "x-custom header missing: {headers:?}"
    );
    assert_eq!(
        headers
            .get("x-request-id")
            .map(|v| v.to_str().unwrap_or("")),
        Some("abc-123"),
        "x-request-id header missing: {headers:?}"
    );

    server.stop().await;
}
