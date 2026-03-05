//! Axum ↔ Python handler bridge.
//!
//! Wires bound routes into the axum router and delegates request handling
//! to the appropriate [`dispatch::HandlerDispatch`] implementation.

pub mod asgi;
pub mod context;
pub mod dispatch;

pub mod asgi_dispatch;
pub mod plan_executor;
pub mod streaming;

use crate::route::{BoundRoute, DispatchStrategy, HandlerKind, HttpMethod};
use crate::runtime::lifecycle::LifecycleCache;
use asgi_dispatch::AsgiBridgeDispatch;
use axum::extract::ConnectInfo;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use dispatch::{AppState, HandlerDispatch, RequestResponseDispatch};
use plan_executor::PlanExecutorDispatch;
use std::collections::HashSet;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Default request timeout. Prevents slow clients from holding workers indefinitely.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default max concurrent requests per worker.
///
/// Each worker has one GIL. Even though we release the GIL during `await`,
/// Python bytecode execution is serial. This limit prevents GIL thrashing.
const DEFAULT_CONCURRENCY_LIMIT: usize = 16;

/// Per-route state baked into the axum handler via `.with_state()`.
#[derive(Clone)]
struct HandlerState {
    route: Arc<BoundRoute>,
    app_state: Arc<AppState>,
    dispatch: Arc<dyn HandlerDispatch>,
    server_addr: SocketAddr,
}

/// The axum handler function — transport boundary.
///
/// Converts axum types to transport-neutral [`InboundRequest`] once here.
/// Everything below is transport-agnostic.
async fn python_handler(
    axum::extract::State(state): axum::extract::State<HandlerState>,
    raw_params: axum::extract::RawPathParams,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, crate::error::AppError> {
    let path_params = collect_path_params(&raw_params);

    // Transport boundary: axum → InboundRequest (once, here)
    let inbound = crate::transport::convert::from_axum_request(
        request,
        path_params,
        state.server_addr,
        Some(client_addr),
    );

    // Everything below is transport-agnostic
    let response = state
        .dispatch
        .handle(
            Arc::clone(&state.route),
            Arc::clone(&state.app_state),
            inbound,
        )
        .await?;

    // Convert back at the boundary
    Ok(crate::transport::convert::to_axum_response(response))
}

/// Collect path params from axum's `RawPathParams` extractor.
fn collect_path_params(raw_params: &axum::extract::RawPathParams) -> Vec<(String, String)> {
    raw_params
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Select the dispatch impl based on the route's dispatch strategy.
///
/// Routes with a compiled dependency plan (that don't need ASGI) use
/// [`PlanExecutorDispatch`]. Otherwise falls back to dispatch strategy.
fn dispatch_for(
    route: &BoundRoute,
    lifecycle_cache: &Arc<LifecycleCache>,
) -> Arc<dyn HandlerDispatch> {
    if let Some(plan) = &route.manifest.dependency_plan
        && !plan.needs_asgi
    {
        return Arc::new(PlanExecutorDispatch::new(Arc::clone(lifecycle_cache)));
    }
    match route.manifest.dispatch_strategy {
        DispatchStrategy::Direct => Arc::new(RequestResponseDispatch),
        DispatchStrategy::AsgiBridge => Arc::new(AsgiBridgeDispatch),
    }
}

// ── WebSocket handler ────────────────────────────────────────────────────

/// Per-route state for WebSocket handlers.
#[derive(Clone)]
struct WsHandlerState {
    route: Arc<BoundRoute>,
    server_addr: SocketAddr,
}

/// axum handler for WebSocket upgrade.
async fn ws_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<WsHandlerState>,
    raw_params: axum::extract::RawPathParams,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let path_params = collect_path_params(&raw_params);
    let inbound = crate::transport::convert::from_axum_request(
        request,
        path_params,
        state.server_addr,
        Some(client_addr),
    );

    ws.on_upgrade(move |socket| handle_ws_connection(socket, state, inbound))
}

/// WebSocket send channel buffer size.
const WS_CHANNEL_SIZE: usize = 32;

/// Bridge an axum WebSocket to a Python ASGI handler.
async fn handle_ws_connection(
    socket: axum::extract::ws::WebSocket,
    state: WsHandlerState,
    inbound: crate::transport::types::InboundRequest,
) {
    use futures_util::StreamExt;
    use tokio::sync::mpsc;

    let (ws_tx, ws_rx) = socket.split();

    let (incoming_tx, incoming_rx) = mpsc::channel(WS_CHANNEL_SIZE);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(WS_CHANNEL_SIZE);

    let recv_task = tokio::spawn(forward_ws_incoming(ws_rx, incoming_tx));
    let send_task = tokio::spawn(forward_ws_outgoing(outgoing_rx, ws_tx));

    let receive = asgi::AsgiWsReceive::new(incoming_rx);
    let send = asgi::AsgiSend::new(outgoing_tx);

    let result = launch_ws_handler(&state, &inbound, receive, send);
    match result {
        Ok(future) => {
            if let Err(e) = future.await {
                tracing::error!(error = %e, "websocket handler error");
            }
        }
        Err(e) => tracing::error!(error = %e, "websocket handler setup error"),
    }

    recv_task.abort();
    send_task.abort();
}

/// Build ASGI scope and start the Python WebSocket handler coroutine.
///
/// Acquires the GIL briefly to construct scope/receive/send and call the
/// handler. Returns a Rust future that can be awaited with the GIL released.
fn launch_ws_handler(
    state: &WsHandlerState,
    inbound: &crate::transport::types::InboundRequest,
    receive: asgi::AsgiWsReceive,
    send: asgi::AsgiSend,
) -> Result<impl Future<Output = Result<pyo3::Py<pyo3::PyAny>, pyo3::PyErr>>, crate::error::AppError>
{
    pyo3::Python::attach(|py| {
        let scope = asgi::build_ws_scope(py, inbound)
            .map_err(|e| crate::error::AppError::Internal(format!("build ws scope: {e}")))?;
        let receive_obj = pyo3::Py::new(py, receive)
            .map_err(|e| crate::error::AppError::Internal(format!("wrap ws receive: {e}")))?;
        let send_obj = pyo3::Py::new(py, send)
            .map_err(|e| crate::error::AppError::Internal(format!("wrap ws send: {e}")))?;
        let coro = state
            .route
            .handler
            .call(py, (scope, receive_obj, send_obj), None)
            .map_err(|e| crate::error::AppError::Internal(format!("ws handler call: {e}")))?;
        pyo3_async_runtimes::tokio::into_future(coro.into_bound(py))
            .map_err(|e| crate::error::AppError::Internal(format!("ws into_future: {e}")))
    })
}

/// Forward incoming WebSocket frames from axum to the Python receive channel.
async fn forward_ws_incoming(
    mut ws_rx: futures_util::stream::SplitStream<axum::extract::ws::WebSocket>,
    incoming_tx: tokio::sync::mpsc::Sender<asgi::WsIncomingEvent>,
) {
    use futures_util::StreamExt;

    if incoming_tx
        .send(asgi::WsIncomingEvent::Connect)
        .await
        .is_err()
    {
        return;
    }

    while let Some(result) = ws_rx.next().await {
        let Ok(msg) = result else { break };
        let is_close = matches!(msg, axum::extract::ws::Message::Close(_));
        let event = match msg {
            axum::extract::ws::Message::Text(t) => asgi::WsIncomingEvent::Receive {
                text: Some(t.to_string()),
                bytes: None,
            },
            axum::extract::ws::Message::Binary(b) => asgi::WsIncomingEvent::Receive {
                text: None,
                bytes: Some(b.to_vec()),
            },
            axum::extract::ws::Message::Close(frame) => {
                let code = frame.map_or(1000, |f| f.code);
                asgi::WsIncomingEvent::Disconnect { code }
            }
            _ => continue,
        };
        if incoming_tx.send(event).await.is_err() {
            break;
        }
        if is_close {
            break;
        }
    }
}

/// Forward outgoing ASGI events to the axum WebSocket sender.
async fn forward_ws_outgoing(
    mut outgoing_rx: tokio::sync::mpsc::Receiver<asgi::AsgiEvent>,
    mut ws_tx: futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
) {
    use futures_util::SinkExt;

    while let Some(event) = outgoing_rx.recv().await {
        match event {
            asgi::AsgiEvent::WsSend {
                text: Some(t),
                bytes: _,
            } => {
                if ws_tx
                    .send(axum::extract::ws::Message::Text(t.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            asgi::AsgiEvent::WsSend {
                text: None,
                bytes: Some(b),
            } => {
                if ws_tx
                    .send(axum::extract::ws::Message::Binary(b.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            asgi::AsgiEvent::WsClose { .. } => {
                let _ = ws_tx.close().await;
                break;
            }
            _ => {}
        }
    }
}

/// Register built-in health probes, skipping paths the user already registered.
fn register_health_probes(mut router: Router, user_paths: &HashSet<&str>) -> Router {
    if !user_paths.contains("/healthz") {
        router = router.route(
            "/healthz",
            get(|| async { Json(serde_json::json!({"status": "alive"})) }),
        );
    }
    if !user_paths.contains("/readyz") {
        router = router.route(
            "/readyz",
            get(|| async { Json(serde_json::json!({"status": "ready"})) }),
        );
    }
    router
}

/// Wire bound routes into the axum router.
fn register_routes(
    mut router: Router,
    routes: Vec<BoundRoute>,
    app_state: &Arc<AppState>,
    server_addr: SocketAddr,
    lifecycle_cache: &Arc<LifecycleCache>,
) -> Router {
    for route in routes {
        let path = route.manifest.path.as_str().to_owned();

        if route.manifest.kind == HandlerKind::WebSocket {
            let ws_state = WsHandlerState {
                route: Arc::new(route),
                server_addr,
            };
            router = router.route(&path, get(ws_handler).with_state(ws_state));
            continue;
        }

        let dispatch = dispatch_for(&route, lifecycle_cache);
        let method = route.manifest.method;
        let state = HandlerState {
            route: Arc::new(route),
            app_state: Arc::clone(app_state),
            dispatch,
            server_addr,
        };

        let method_router = match method {
            HttpMethod::Get => get(python_handler),
            HttpMethod::Post => post(python_handler),
            HttpMethod::Put => put(python_handler),
            HttpMethod::Delete => delete(python_handler),
            HttpMethod::Patch => patch(python_handler),
        };

        router = router.route(&path, method_router.with_state(state));
    }
    router
}

/// Build the axum Router from bound routes (without tower layer wrapping).
///
/// Returns a bare `Router` — layer wrapping happens in [`wrap_layers`].
pub fn build_router(
    routes: Vec<BoundRoute>,
    app_state: Arc<AppState>,
    server_addr: SocketAddr,
    lifecycle_cache: Arc<LifecycleCache>,
) -> Router {
    let user_paths: HashSet<&str> = routes.iter().map(|r| r.manifest.path.as_str()).collect();
    let router = register_health_probes(Router::new(), &user_paths);
    register_routes(router, routes, &app_state, server_addr, &lifecycle_cache)
}

/// Apply tower layer stack to the router.
///
/// Each layer is applied individually via `Router::layer()` so axum handles
/// body type conversions internally. Layers run outermost-first (bottom of
/// chain runs first on incoming requests).
///
/// Note: `NormalizePath` is not applied here because it produces an opaque
/// type incompatible with `axum::serve`. Trailing slash normalization can
/// be added via a custom axum middleware in the future.
pub fn wrap_layers(router: Router, request_timeout: Option<Duration>) -> Router {
    use axum::error_handling::HandleErrorLayer;
    use tower_http::cors::CorsLayer;
    use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
    use tower_http::trace::TraceLayer;

    let timeout = request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);

    // Fallible layers (timeout, concurrency) must be wrapped in HandleErrorLayer
    // to convert their errors to responses before axum sees them.
    router
        .layer(
            tower::ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_infra_error))
                .layer(tower::timeout::TimeoutLayer::new(timeout))
                .layer(tower::limit::ConcurrencyLimitLayer::new(
                    DEFAULT_CONCURRENCY_LIMIT,
                ))
                .into_inner(),
        )
        // Infallible layers applied directly.
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(CorsLayer::permissive())
}

/// Convert tower infrastructure errors (timeout, concurrency limit) to responses.
async fn handle_infra_error(err: tower::BoxError) -> axum::response::Response {
    use axum::response::IntoResponse;

    if err.is::<tower::timeout::error::Elapsed>() {
        return crate::error::AppError::Timeout.into_response();
    }

    // Concurrency limit exceeded → 503
    (
        http::StatusCode::SERVICE_UNAVAILABLE,
        "Service temporarily unavailable",
    )
        .into_response()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    use crate::route::{HandlerKind, QualName, ResponseType, RouteManifest, RoutePath};
    use crate::with_py;

    fn make_route_with_strategy(strategy: DispatchStrategy) -> BoundRoute {
        with_py(|py| BoundRoute {
            manifest: RouteManifest {
                kind: HandlerKind::RequestResponse,
                method: HttpMethod::Get,
                path: RoutePath::new("/test").unwrap(),
                handler_qualname: QualName::new("test.handler").unwrap(),
                params: Vec::new(),
                response_type: ResponseType::RawResponse,
                tags: Vec::new(),
                dispatch_strategy: strategy,
                dependency_plan: None,
                status_code: 200,
                summary: None,
                description: None,
                include_in_schema: true,
                deprecated: false,
                operation_id: None,
            },
            handler: py.None(),
            params: Vec::new(),
            response_model: None,
            has_body_param: false,
            dependant: None,
            fastapi_app: None,
        })
    }

    #[test]
    fn dispatch_for_direct() {
        let route = make_route_with_strategy(DispatchStrategy::Direct);
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("RequestResponseDispatch"));
    }

    #[test]
    fn dispatch_for_asgi_bridge() {
        let route = make_route_with_strategy(DispatchStrategy::AsgiBridge);
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[test]
    fn dispatch_for_plan_executor() {
        use crate::route::DependencyPlan;
        let mut route = make_route_with_strategy(DispatchStrategy::Direct);
        route.manifest.dependency_plan = Some(DependencyPlan {
            steps: Vec::new(),
            handler_kwargs: Vec::new(),
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        });
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("PlanExecutorDispatch"));
    }

    #[test]
    fn dispatch_for_plan_needs_asgi_falls_back() {
        use crate::route::DependencyPlan;
        let mut route = make_route_with_strategy(DispatchStrategy::AsgiBridge);
        route.manifest.dependency_plan = Some(DependencyPlan {
            steps: Vec::new(),
            handler_kwargs: Vec::new(),
            needs_asgi: true,
            generator_cleanup_indices: Vec::new(),
        });
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[tokio::test]
    async fn handle_infra_error_timeout() {
        let err: tower::BoxError = Box::new(tower::timeout::error::Elapsed::new());
        let resp = handle_infra_error(err).await;
        assert_eq!(resp.status(), http::StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn handle_infra_error_other() {
        let err: tower::BoxError = Box::new(std::io::Error::other("overload"));
        let resp = handle_infra_error(err).await;
        assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn register_health_probes_adds_endpoints() {
        let router = register_health_probes(Router::new(), &HashSet::new());

        let req = http::Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);

        let req = http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn register_health_probes_skips_existing() {
        let mut user_paths = HashSet::new();
        user_paths.insert("/healthz");
        let router = register_health_probes(Router::new(), &user_paths);

        // /healthz should NOT be registered (user already has it)
        let req = http::Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);

        // /readyz should still be registered
        let req = http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    // ── SSE / WebSocket dispatch tests ──────────────────────────────────

    fn make_route_with_kind(kind: HandlerKind) -> BoundRoute {
        with_py(|py| {
            let dispatch = match kind {
                HandlerKind::WebSocket | HandlerKind::SSE => DispatchStrategy::AsgiBridge,
                HandlerKind::RequestResponse => DispatchStrategy::Direct,
            };
            BoundRoute {
                manifest: RouteManifest {
                    kind,
                    method: HttpMethod::Get,
                    path: RoutePath::new("/test").unwrap(),
                    handler_qualname: QualName::new("test.handler").unwrap(),
                    params: Vec::new(),
                    response_type: ResponseType::RawResponse,
                    tags: Vec::new(),
                    dispatch_strategy: dispatch,
                    dependency_plan: None,
                    status_code: 200,
                    summary: None,
                    description: None,
                    include_in_schema: true,
                    deprecated: false,
                    operation_id: None,
                },
                handler: py.None(),
                params: Vec::new(),
                response_model: None,
                has_body_param: false,
                dependant: None,
                fastapi_app: None,
            }
        })
    }

    #[test]
    fn dispatch_for_sse_uses_asgi_bridge() {
        let route = make_route_with_kind(HandlerKind::SSE);
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[test]
    fn dispatch_for_websocket_uses_asgi_bridge() {
        let route = make_route_with_kind(HandlerKind::WebSocket);
        let cache = Arc::new(LifecycleCache::empty());
        let d = dispatch_for(&route, &cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[test]
    fn register_ws_route_uses_get() {
        // Verify a WebSocket route is registered (doesn't crash)
        // and goes through the WS path (different state type)
        let ws_route = make_route_with_kind(HandlerKind::WebSocket);
        let app_state = Arc::new(AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
        });
        let server_addr = SocketAddr::from(([127, 0, 0, 1], 8080));
        let cache = Arc::new(LifecycleCache::empty());

        // Should not panic — verifies WS routes are registered correctly
        let _router = register_routes(
            Router::new(),
            vec![ws_route],
            &app_state,
            server_addr,
            &cache,
        );
    }
}
