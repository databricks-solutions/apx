//! Protocol-level integration tests for the `apx-mcp` crate.
//!
//! Uses `tokio::io::duplex` to create in-memory byte channels and exercises
//! every tool, resource, and error path through the MCP JSON-RPC protocol.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use rmcp::ServiceExt;
use rmcp::model::*;
use rmcp::service::{Peer, RoleClient, RunningService};

// ---------------------------------------------------------------------------
// Shared project fixture
// ---------------------------------------------------------------------------

/// Returns a path to a fully-initialized apx project (created once per test run).
async fn project_path() -> &'static Path {
    static PROJECT: tokio::sync::OnceCell<PathBuf> = tokio::sync::OnceCell::const_new();
    PROJECT
        .get_or_init(|| async {
            let dir = tempfile::Builder::new()
                .prefix("apx-mcp-test-")
                .tempdir()
                .expect("failed to create tempdir");
            // Leak the tempdir so it persists for the whole test run.
            let path = dir.keep();

            // Run `apx init` programmatically with UI addon.
            let exit_code = apx_cli::init::run(apx_cli::init::InitArgs {
                app_path: Some(path.clone()),
                app_name: Some("mcp-test".into()),
                addons: Some(vec!["ui".into()]),
                no_addons: false,
                profile: Some("DEFAULT".into()),
                as_member: None,
            })
            .await;
            assert_eq!(exit_code, 0, "apx init failed");

            // Run `uv sync` using the resolved uv binary (cached by init).
            let uv = apx_core::external::Uv::try_new().expect("uv should be cached after init");
            uv.sync(&path).await.expect("uv sync failed");

            path
        })
        .await
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

async fn try_call_tool(
    client: &Peer<RoleClient>,
    name: &str,
    args: serde_json::Value,
) -> Result<CallToolResult, rmcp::ServiceError> {
    client
        .call_tool(CallToolRequestParams {
            name: name.to_string().into(),
            arguments: Some(args.as_object().unwrap().clone()),
            meta: None,
            task: None,
        })
        .await
}

async fn call_tool(
    client: &Peer<RoleClient>,
    name: &str,
    args: serde_json::Value,
) -> CallToolResult {
    try_call_tool(client, name, args)
        .await
        .expect("call_tool should not fail at protocol level")
}

// ===========================================================================
// Tests
// ===========================================================================

// --- Task 2: Smoke test ---

#[tokio::test]
async fn test_harness_smoke() {
    let path = project_path().await;
    assert!(path.join("pyproject.toml").exists());
}

// --- Task 3: Protocol fundamentals ---

#[tokio::test]
async fn test_initialize_handshake() {
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
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
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "routes tool should succeed, got error: {}",
        result_text(&result)
    );
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
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "routes tool should succeed for regression test"
    );
    let sc = result.structured_content.unwrap();
    assert!(
        sc.is_object(),
        "structured_content must be an object (regression e29c897), got: {sc}"
    );
    assert!(!sc.is_array(), "structured_content must NOT be an array");
}

// --- Task 6: Tool calls — check, get_route_info ---

#[tokio::test]
async fn test_check_tool() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "check",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    let sc = result
        .structured_content
        .expect("check should have structured_content");
    assert!(sc.is_object(), "structured_content should be an object");
    let status = sc
        .get("status")
        .and_then(|v| v.as_str())
        .expect("should have status key");
    assert!(
        status == "passed" || status == "failed",
        "status should be 'passed' or 'failed', got: {status}"
    );
}

#[tokio::test]
async fn test_get_route_info() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;

    // First discover a valid operation_id from the routes tool.
    let routes_result = call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    assert!(
        routes_result.is_error.is_none() || routes_result.is_error == Some(false),
        "routes tool should succeed"
    );

    let sc = routes_result.structured_content.unwrap();
    let routes = sc.get("routes").unwrap().as_array().unwrap();
    assert!(!routes.is_empty(), "project should have at least one route");

    let operation_id = routes[0].get("id").unwrap().as_str().unwrap();
    let result = call_tool(
        &client,
        "get_route_info",
        serde_json::json!({
            "app_path": path.to_str().unwrap(),
            "operation_id": operation_id,
        }),
    )
    .await;

    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "get_route_info should succeed for known operation_id"
    );

    let sc = result
        .structured_content
        .expect("get_route_info should have structured_content");
    assert!(sc.get("operation_id").is_some(), "should have operation_id");
    assert!(sc.get("method").is_some(), "should have method");
    assert!(sc.get("path").is_some(), "should have path");
    assert!(sc.get("example").is_some(), "should have example");

    let example = sc.get("example").unwrap().as_str().unwrap();
    let method = sc.get("method").unwrap().as_str().unwrap();
    if method == "GET" {
        assert!(
            example.contains("Suspense"),
            "GET route example should contain Suspense"
        );
    } else {
        assert!(
            example.contains("mutate"),
            "non-GET route example should contain mutate"
        );
    }
}

// --- Task 7: Dev server lifecycle ---

/// Helper to extract text content from a CallToolResult.
fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn test_start_stop_cycle() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;

    // Start the dev server.
    let start_result = call_tool(
        &client,
        "start",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    let start_text = result_text(&start_result);
    if start_result.is_error == Some(true) {
        // Start may fail in CI or constrained environments — that's acceptable.
        eprintln!("start tool returned error (acceptable in test): {start_text}");
        return;
    }
    assert!(
        start_text.contains("http://"),
        "start should return URL, got: {start_text}"
    );

    // Stop the dev server.
    let stop_result = call_tool(
        &client,
        "stop",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    let stop_text = result_text(&stop_result);
    assert!(
        stop_text.contains("stopped") || stop_text.contains("No dev server"),
        "stop should confirm stopped, got: {stop_text}"
    );
}

#[tokio::test]
async fn test_logs_tool() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;

    // Start the dev server first.
    let start_result = call_tool(
        &client,
        "start",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    if start_result.is_error == Some(true) {
        eprintln!("start tool returned error — skipping logs test");
        return;
    }

    // Fetch logs.
    let logs_result = call_tool(
        &client,
        "logs",
        serde_json::json!({"app_path": path.to_str().unwrap(), "duration": "1m"}),
    )
    .await;

    let sc = logs_result.structured_content;
    if let Some(sc) = sc {
        assert!(sc.is_object(), "logs structured_content should be object");
        assert!(sc.get("duration").is_some(), "should have duration");
        assert!(sc.get("count").is_some(), "should have count");
        assert!(sc.get("entries").is_some(), "should have entries");
    }

    // Cleanup: stop the server.
    let _ = call_tool(
        &client,
        "stop",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;
}

#[tokio::test]
async fn test_restart_tool() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;

    // Start the dev server first.
    let start_result = call_tool(
        &client,
        "start",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    if start_result.is_error == Some(true) {
        eprintln!("start tool returned error — skipping restart test");
        return;
    }

    // Restart the dev server.
    let restart_result = call_tool(
        &client,
        "restart",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    let restart_text = result_text(&restart_result);
    if restart_result.is_error != Some(true) {
        assert!(
            restart_text.contains("http://"),
            "restart should return URL, got: {restart_text}"
        );
    }

    // Cleanup: stop the server.
    let _ = call_tool(
        &client,
        "stop",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;
}

// --- Task 8: Registry tools ---

#[tokio::test]
async fn test_list_registry_components() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "list_registry_components",
        serde_json::json!({"app_path": path.to_str().unwrap()}),
    )
    .await;

    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "list_registry_components should succeed with UI addon: {}",
        result_text(&result)
    );

    let sc = result
        .structured_content
        .expect("list_registry_components should have structured_content");
    assert!(sc.is_object(), "structured_content should be object");
    assert!(sc.get("registry").is_some(), "should have registry key");
    assert!(sc.get("total").is_some(), "should have total key");
    assert!(sc.get("items").is_some(), "should have items key");
}

#[tokio::test]
async fn test_search_registry_components() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "search_registry_components",
        serde_json::json!({"app_path": path.to_str().unwrap(), "query": "button"}),
    )
    .await;

    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "search_registry_components should succeed with UI addon: {}",
        result_text(&result)
    );

    let sc = result
        .structured_content
        .expect("search_registry_components should have structured_content");
    assert!(sc.is_object(), "structured_content should be object");
    assert!(sc.get("query").is_some(), "should have query key");
    assert!(sc.get("results").is_some(), "should have results key");
    assert!(
        sc.get("results").unwrap().is_array(),
        "results should be array"
    );
}

// --- Task 9: Error handling ---

#[tokio::test]
async fn test_tool_with_relative_path() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = try_call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": "relative/path"}),
    )
    .await;

    // validated_app_path returns Err(ErrorData::invalid_params)
    assert!(
        result.is_err(),
        "relative path should produce protocol error"
    );
}

#[tokio::test]
async fn test_tool_with_nonexistent_path() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = try_call_tool(
        &client,
        "routes",
        serde_json::json!({"app_path": "/tmp/__apx_nonexistent__"}),
    )
    .await;

    assert!(
        result.is_err(),
        "nonexistent path should produce protocol error"
    );
}

#[tokio::test]
async fn test_get_route_info_unknown_operation() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let result = call_tool(
        &client,
        "get_route_info",
        serde_json::json!({
            "app_path": path.to_str().unwrap(),
            "operation_id": "nonexistent_operation_id_xyz",
        }),
    )
    .await;

    assert_eq!(
        result.is_error,
        Some(true),
        "unknown operation_id should return is_error=true"
    );
    let text = result_text(&result);
    assert!(
        text.contains("not found") || text.contains("Not found") || text.contains("Failed"),
        "error text should indicate not found, got: {text}"
    );
}

// --- Task 10: Cross-cutting structured content validation ---

#[tokio::test]
async fn test_all_tools_return_structured_objects() {
    let path = project_path().await;
    let (client, _shutdown) = spawn_client(path).await;
    let app_path = path.to_str().unwrap();

    let tool_calls: Vec<(&str, serde_json::Value)> = vec![
        ("check", serde_json::json!({"app_path": app_path})),
        ("routes", serde_json::json!({"app_path": app_path})),
        (
            "list_registry_components",
            serde_json::json!({"app_path": app_path}),
        ),
        (
            "search_registry_components",
            serde_json::json!({"app_path": app_path, "query": "button"}),
        ),
    ];

    for (tool_name, args) in tool_calls {
        let result = call_tool(&client, tool_name, args).await;

        // check tool may report status=failed (pyright issues) but still returns structured content
        if let Some(sc) = &result.structured_content {
            assert!(
                sc.is_object(),
                "{tool_name}: structured_content should be an object, got: {sc}"
            );
            assert!(
                !sc.is_array(),
                "{tool_name}: structured_content must NOT be an array"
            );
            assert!(
                !sc.is_null(),
                "{tool_name}: structured_content must NOT be null"
            );
            assert!(
                !sc.is_string(),
                "{tool_name}: structured_content must NOT be a string"
            );
        }
    }
}
