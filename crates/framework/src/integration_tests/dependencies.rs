//! `Depends()`, generator dependencies, background tasks, and dependency override tests.

use super::TestServer;

/// `Depends()` routes go through ASGI bridge (FastAPI's `solve_dependencies`).
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

/// Sync generator dependency with `yield` + `finally` teardown.
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

/// Async generator dependency with `yield` + `finally`.
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

/// Chain: `get_settings() → get_db(settings) → handler(db)`.
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

/// Two independent `Depends()` params on one endpoint.
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

/// Class with `__call__` used as dependency callable.
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

/// Context manager class inside a sync generator dependency.
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

/// `app.dependency_overrides` — mock injection.
#[tokio::test]
async fn dependency_overrides() {
    let app = r#"
from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    return {"source": "real_db"}

def mock_db():
    return {"source": "mock"}

@app.get("/db")
async def read_db(db=Depends(get_db)):
    return db

app.dependency_overrides[get_db] = mock_db
"#;

    let mut server = TestServer::start(app, "_apx_test_dep_override").await;

    let (status, body) = server.get("/db").await;
    assert_eq!(status, 200, "dep override: {body}");
    assert_eq!(body["source"], "mock");

    server.stop().await;
}

// ── Background tasks ─────────────────────────────────────────────────────

/// `BackgroundTasks` param resolved through ASGI bridge.
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

/// `BackgroundTasks` with async task function.
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

/// `BackgroundTasks` injected into a dependency.
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
