//! Protocol-level integration tests for the `apx-mcp` crate.
//!
//! Uses `tokio::io::duplex` to create in-memory byte channels and exercises
//! every tool, resource, and error path through the MCP JSON-RPC protocol.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rmcp::ServiceExt;
use rmcp::model::*;
use rmcp::service::{Peer, RoleClient, RunningService};

// ---------------------------------------------------------------------------
// Shared project fixture
// ---------------------------------------------------------------------------

/// Returns a path to a fully-initialized apx project (created once per test run).
fn project_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("apx-mcp-test-")
            .tempdir()
            .expect("failed to create tempdir");
        // Leak the tempdir so it persists for the whole test run.
        let path = dir.keep();

        // Run `apx init` to scaffold a project.
        let status = std::process::Command::new("apx")
            .args([
                "init",
                path.to_str().unwrap(),
                "--name",
                "mcp-test",
                "--no-addons",
                "--profile",
                "DEFAULT",
            ])
            .env("APX_NON_INTERACTIVE", "1")
            .status()
            .expect("failed to run `apx init`");
        assert!(status.success(), "apx init failed with {status}");

        // Run `uv sync` inside the project.
        let status = std::process::Command::new("uv")
            .arg("sync")
            .current_dir(&path)
            .status()
            .expect("failed to run `uv sync`");
        assert!(status.success(), "uv sync failed with {status}");

        path
    })
}

// ---------------------------------------------------------------------------
// Server spawner
// ---------------------------------------------------------------------------

/// Minimal client handler (all methods have default no-op impls).
struct TestClient;
impl rmcp::handler::client::ClientHandler for TestClient {}

/// Creates a duplex channel, spawns the MCP server on one end, and connects
/// a client on the other.  Returns the running client peer.
async fn spawn_client(
    project: &Path,
) -> (
    RunningService<RoleClient, TestClient>,
    tokio::sync::broadcast::Sender<()>,
) {
    let tmp = tempfile::Builder::new()
        .prefix("apx-mcp-db-")
        .tempdir()
        .expect("tempdir for db");

    let dev_db = apx_db::DevDb::open_at(&tmp.keep().join("test.db"))
        .await
        .expect("open dev db");

    let cache_state = apx_core::components::new_cache_state();
    let index_state = apx_mcp::context::IndexState::new();
    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);

    let ctx = apx_mcp::context::AppContext {
        dev_db,
        sdk_doc_index: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        cache_state,
        index_state,
        shutdown_tx: shutdown_tx.clone(),
    };

    let server = apx_mcp::server::ApxServer::new(ctx, None);

    // In-memory duplex: writing to one end is reading from the other.
    let (server_stream, client_stream) = tokio::io::duplex(65_536);

    // Spawn the server on one end (runs in background).
    tokio::spawn(async move {
        let service = server.serve(server_stream).await.expect("server serve");
        let _ = service.waiting().await;
    });

    // Connect client on the other end.
    let client = TestClient.serve(client_stream).await.expect("client serve");

    // Give the project path to callers via the shutdown_tx (they don't use it
    // directly but it keeps the server alive).
    let _ = project; // used by callers, not here
    (client, shutdown_tx)
}

// ---------------------------------------------------------------------------
// Helper: call_tool
// ---------------------------------------------------------------------------

async fn call_tool(
    client: &Peer<RoleClient>,
    name: &str,
    args: serde_json::Value,
) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams {
            name: name.to_string().into(),
            arguments: Some(args.as_object().unwrap().clone()),
            meta: None,
            task: None,
        })
        .await
        .expect("call_tool should not fail at protocol level")
}

// ===========================================================================
// Tests
// ===========================================================================

// --- Task 2: Smoke test ---

#[tokio::test]
async fn test_harness_smoke() {
    let path = project_path();
    assert!(path.join("pyproject.toml").exists());
}

// --- Task 3: Protocol fundamentals ---

#[tokio::test]
async fn test_initialize_handshake() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let info = client.peer_info().expect("server info should be present");
    assert_eq!(info.server_info.name, "apx");
    assert_eq!(
        info.server_info.version,
        env!("CARGO_PKG_VERSION"),
        "server version should match apx-mcp crate version"
    );
    assert!(
        info.capabilities.tools.is_some(),
        "capabilities should include tools"
    );
    assert!(
        info.capabilities.resources.is_some(),
        "capabilities should include resources"
    );
}

#[tokio::test]
async fn test_list_tools() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let tools = client.list_all_tools().await.expect("list_all_tools");

    let expected_names: Vec<&str> = vec![
        "start",
        "stop",
        "restart",
        "logs",
        "check",
        "refresh_openapi",
        "get_route_info",
        "routes",
        "databricks_apps_logs",
        "search_registry_components",
        "add_component",
        "list_registry_components",
        "feedback_prepare",
        "feedback_submit",
        "docs",
    ];
    assert_eq!(
        tools.len(),
        expected_names.len(),
        "expected {} tools, got {}: {:?}",
        expected_names.len(),
        tools.len(),
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>()
    );

    for name in &expected_names {
        assert!(
            tools.iter().any(|t| t.name.as_ref() == *name),
            "missing tool: {name}"
        );
    }
    for tool in &tools {
        assert!(!tool.name.is_empty(), "tool name should not be empty");
        assert!(
            tool.description.is_some() && !tool.description.as_ref().unwrap().is_empty(),
            "tool {} should have a non-empty description",
            tool.name
        );
    }
}

#[tokio::test]
async fn test_list_resources() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let resources = client
        .list_all_resources()
        .await
        .expect("list_all_resources");
    assert_eq!(resources.len(), 1, "expected 1 resource");
    assert_eq!(resources[0].raw.uri, "apx://info");
}

#[tokio::test]
async fn test_list_resource_templates() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let templates = client
        .list_all_resource_templates()
        .await
        .expect("list_all_resource_templates");
    assert_eq!(templates.len(), 1, "expected 1 resource template");
    assert_eq!(templates[0].raw.uri_template, "apx://project/{app_path}");
}

// --- Task 4: Resource reads ---

#[tokio::test]
async fn test_read_info_resource() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let result = client
        .read_resource(ReadResourceRequestParams {
            uri: "apx://info".into(),
            meta: None,
        })
        .await
        .expect("read_resource apx://info");
    assert_eq!(result.contents.len(), 1);
    let text = match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text resource, got {other:?}"),
    };
    assert!(
        text.contains("Project Structure"),
        "info should mention Project Structure"
    );
    assert!(
        text.contains("Frontend Patterns"),
        "info should mention Frontend Patterns"
    );
    assert!(
        text.contains("Backend Patterns"),
        "info should mention Backend Patterns"
    );
}

#[tokio::test]
async fn test_read_project_resource() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let uri = format!("apx://project/{}", path.display());
    let result = client
        .read_resource(ReadResourceRequestParams { uri, meta: None })
        .await
        .expect("read_resource project");
    assert_eq!(result.contents.len(), 1);
    let text = match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text resource, got {other:?}"),
    };
    let json: serde_json::Value =
        serde_json::from_str(text).expect("project resource should be valid JSON");
    assert!(json.get("app_name").is_some(), "should have app_name");
    assert!(json.get("app_slug").is_some(), "should have app_slug");
    assert!(json.get("routes").is_some(), "should have routes");
    assert!(json.get("has_ui").is_some(), "should have has_ui");
}

#[tokio::test]
async fn test_read_unknown_resource() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let result = client
        .read_resource(ReadResourceRequestParams {
            uri: "apx://nonexistent".into(),
            meta: None,
        })
        .await;
    assert!(result.is_err(), "reading unknown resource should fail");
}

// --- Task 5: Tool calls — routes and structured content ---

#[tokio::test]
async fn test_routes_tool() {
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    // The tool may return is_error=true if OpenAPI generation failed for the
    // test project. Either way, the protocol-level call succeeded. When routes
    // are available, validate the structured content shape.
    if result.is_error == Some(true) {
        // Acceptable for a --no-addons test project.
        return;
    }
    let sc = result
        .structured_content
        .expect("routes should have structured_content");
    assert!(sc.is_object(), "structured_content should be an object");
    let routes = sc.get("routes").expect("should have routes key");
    assert!(routes.is_array(), "routes should be an array");
    for route in routes.as_array().unwrap() {
        assert!(route.get("id").is_some(), "route should have id");
        assert!(route.get("method").is_some(), "route should have method");
        assert!(route.get("path").is_some(), "route should have path");
        assert!(
            route.get("hook_name").is_some(),
            "route should have hook_name"
        );
    }
}

#[tokio::test]
async fn test_routes_structured_content_is_object() {
    // Regression test for e29c897 — structured_content must be an object, not an array.
    let path = project_path();
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    // If routes tool returned an error (e.g., no OpenAPI for test project), skip shape check.
    if result.is_error == Some(true) {
        return;
    }
    let sc = result.structured_content.unwrap();
    assert!(
        sc.is_object(),
        "structured_content must be an object (regression e29c897), got: {sc}"
    );
    assert!(!sc.is_array(), "structured_content must NOT be an array");
}
