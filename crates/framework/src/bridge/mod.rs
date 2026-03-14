//! Axum ↔ Python handler bridge.
//!
//! Wires bound routes into the axum router and delegates request handling
//! to the appropriate [`dispatch::HandlerDispatch`] implementation.

pub mod asgi;
pub mod context_pool;
pub mod dispatch;

pub mod asgi_dispatch;
pub mod direct_dispatch;
pub mod streaming;

/// Check whether bench-trace instrumentation is enabled (`APX_BENCH_TRACE=1`).
///
/// Evaluated once on first call; zero cost thereafter (single atomic load).
pub fn bench_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("APX_BENCH_TRACE").is_ok())
}

use crate::event_loop::EventLoopHandle;
use crate::route::{BoundRoute, DispatchStrategy, HandlerKind, HttpMethod};
use asgi_dispatch::AsgiBridgeDispatch;
use axum::extract::ConnectInfo;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use direct_dispatch::DirectDispatch;
use dispatch::{AppState, HandlerDispatch};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Default request timeout. Prevents slow clients from holding workers indefinitely.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default max concurrent requests per worker.
///
/// Sized to allow the persistent asyncio event loop to efficiently
/// interleave coroutines at `await` points. The GIL serializes
/// bytecode execution but async I/O multiplexing works well at
/// higher concurrency levels — matching uvicorn's model.
const DEFAULT_CONCURRENCY_LIMIT: usize = 256;

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
    use crate::telemetry::http as thttp;

    let method = request.method().clone();
    let method_str = method.as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let route_path = state.route.manifest.path.as_str().to_owned();
    let http_version = thttp::protocol_version(request.version());

    // Scheme is not set in the URI for plain HTTP servers — default to "http".
    let scheme = request.uri().scheme_str().unwrap_or("http").to_owned();

    tracing::debug!(
        method = %method,
        path = %path,
        dispatch = ?state.dispatch,
        "python_handler entry"
    );

    // Construct the OTEL span name: "{METHOD} {route}" per semconv.
    let span_name = format!("{method_str} {route_path}");

    // OTEL server span — semconv v1.23+ attribute names.
    let span = tracing::info_span!(
        "http.request",
        otel.name = %span_name,
        otel.kind = "server",
        http.request.method = %method_str,
        http.route = %route_path,
        url.path = %path,
        url.query = tracing::field::Empty,
        url.scheme = %scheme,
        server.address = %state.server_addr.ip(),
        server.port = state.server_addr.port(),
        client.address = %client_addr.ip(),
        network.protocol.version = %http_version,
        http.response.status_code = tracing::field::Empty,
        error.type = tracing::field::Empty,
    );
    let _span_guard = span.enter();

    if let Some(ref q) = query {
        span.record("url.query", q.as_str());
    }

    // Metrics: active requests (RAII guard decrements on drop).
    let _active_guard = thttp::ActiveRequestGuard::enter(&method_str, &scheme);
    let start = std::time::Instant::now();

    let path_params = collect_path_params(&raw_params);

    // Transport boundary: axum → InboundRequest (once, here)
    let inbound = crate::transport::convert::from_axum_request(
        request,
        path_params,
        state.server_addr,
        Some(client_addr),
    );

    // Everything below is transport-agnostic.
    // Use match instead of `?` so metrics are always recorded.
    let result = state
        .dispatch
        .handle(
            Arc::clone(&state.route),
            Arc::clone(&state.app_state),
            inbound,
        )
        .await;

    let elapsed = start.elapsed().as_secs_f64();

    match result {
        Ok(response) => {
            let status = response.status().as_u16();
            span.record("http.response.status_code", status);

            // Set error.type for server error responses (semconv: SHOULD for >= 500).
            let error_type = if status >= 500 {
                Some(status.to_string())
            } else {
                None
            };
            if let Some(ref et) = error_type {
                span.record("error.type", et.as_str());
            }
            thttp::record_duration(
                elapsed,
                &method_str,
                &scheme,
                status,
                &route_path,
                error_type.as_deref(),
            );
            Ok(response)
        }
        Err(err) => {
            let status = err.status_code().as_u16();
            let error_type = thttp::error_type_for(&err);
            span.record("http.response.status_code", status);
            span.record("error.type", error_type);
            thttp::record_duration(
                elapsed,
                &method_str,
                &scheme,
                status,
                &route_path,
                Some(error_type),
            );
            Err(err)
        }
    }
}

/// Collect path params from axum's `RawPathParams` extractor.
fn collect_path_params(raw_params: &axum::extract::RawPathParams) -> Vec<(String, String)> {
    raw_params
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Create the dispatch impl for a route.
///
/// Routes with `DispatchStrategy::Direct` bypass ASGI entirely.
/// Routes with `DispatchStrategy::AsgiBridge` use the full ASGI pipeline.
fn dispatch_for(route: &BoundRoute) -> Arc<dyn HandlerDispatch> {
    match route.manifest.dispatch_strategy {
        DispatchStrategy::Direct => Arc::new(DirectDispatch),
        DispatchStrategy::AsgiBridge => Arc::new(AsgiBridgeDispatch),
    }
}

// ── WebSocket handler ────────────────────────────────────────────────────

/// Per-route state for WebSocket handlers.
struct WsHandlerState {
    route: Arc<BoundRoute>,
    server_addr: SocketAddr,
    loop_handle: EventLoopHandle,
    scope_interns: Arc<asgi::ScopeInterns>,
    create_task: pyo3::Py<pyo3::PyAny>,
}

impl Clone for WsHandlerState {
    fn clone(&self) -> Self {
        pyo3::Python::attach(|py| Self {
            route: Arc::clone(&self.route),
            server_addr: self.server_addr,
            loop_handle: self.loop_handle.clone(),
            scope_interns: Arc::clone(&self.scope_interns),
            create_task: self.create_task.clone_ref(py),
        })
    }
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
///
/// All Python work runs on the event loop thread via `schedule_callback`.
/// asyncio owns the coroutine lifecycle. The connection stays alive until
/// the WS handler completes (signaled through a `WsDoneCallback`).
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

    // Completion signal — keeps axum handler alive for connection lifetime.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    // Destructure to avoid partial moves — no GIL acquisition on the tokio thread.
    let WsHandlerState {
        route,
        server_addr: _,
        loop_handle,
        scope_interns,
        create_task,
    } = state;

    let schedule_result = loop_handle.schedule_callback(move |py| {
        let result = (|| -> Result<(), crate::error::AppError> {
            let scope = asgi::build_ws_scope(py, &inbound, &scope_interns)
                .map_err(|e| crate::error::AppError::Internal(format!("build ws scope: {e}")))?;
            let receive = asgi::AsgiWsReceive::new(incoming_rx);
            let send = asgi::AsgiSend::channel(outgoing_tx);
            let receive_obj = pyo3::Py::new(py, receive)
                .map_err(|e| crate::error::AppError::Internal(format!("wrap ws receive: {e}")))?;
            let send_obj = pyo3::Py::new(py, send)
                .map_err(|e| crate::error::AppError::Internal(format!("wrap ws send: {e}")))?;
            let coro = route
                .handler
                .inner()
                .call(py, (scope, receive_obj, send_obj), None)
                .map_err(|e| crate::error::AppError::Internal(format!("ws handler call: {e}")))?;

            // Python owns the coroutine lifecycle — we're on the event loop
            // thread where the GIL is already held.
            let task = create_task
                .call1(py, (coro,))
                .map_err(|e| crate::error::AppError::Internal(format!("create_task: {e}")))?;

            // Signal completion when WS handler finishes.
            let callback = pyo3::Py::new(
                py,
                crate::event_loop::scheduling::WsDoneCallback::new(done_tx),
            )
            .map_err(|e| crate::error::AppError::Internal(format!("ws done callback: {e}")))?;
            let _ = task.call_method1(py, c"add_done_callback", (callback,));

            Ok(())
        })();

        if let Err(e) = result {
            tracing::error!(error = %e, "websocket handler setup error");
            // done_tx drops → done_rx gets RecvError → handler exits → cleanup
        }
    });

    if let Err(e) = schedule_result {
        tracing::error!(error = %e, "websocket schedule_callback failed");
    }

    // Keep connection alive until WS handler completes.
    let _ = done_rx.await;
    recv_task.abort();
    send_task.abort();
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
) -> Router {
    for route in routes {
        let path = route.manifest.path.as_axum_str().into_owned();

        if route.manifest.kind == HandlerKind::WebSocket {
            let ws_state = WsHandlerState {
                route: Arc::new(route),
                server_addr,
                loop_handle: app_state.loop_handle.clone(),
                scope_interns: Arc::clone(&app_state.scope_interns),
                create_task: pyo3::Python::attach(|py| app_state.create_task.clone_ref(py)),
            };
            router = router.route(&path, get(ws_handler).with_state(ws_state));
            continue;
        }

        let dispatch = dispatch_for(&route);
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
) -> Router {
    let user_paths: HashSet<&str> = routes.iter().map(|r| r.manifest.path.as_str()).collect();
    let router = register_health_probes(Router::new(), &user_paths);
    register_routes(router, routes, &app_state, server_addr)
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

    let timeout = request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);

    // Fallible layers (timeout, concurrency) must be wrapped in HandleErrorLayer
    // to convert their errors to responses before axum sees them.
    //
    // Note: UUID request-id and full TraceLayer removed — they add measurable
    // per-request overhead (UUID crypto-random + tracing span creation) without
    // benefit for ASGI dispatch where Python middleware handles observability.
    router.layer(
        tower::ServiceBuilder::new()
            .layer(HandleErrorLayer::new(handle_infra_error))
            .layer(tower::timeout::TimeoutLayer::new(timeout))
            .layer(tower::limit::ConcurrencyLimitLayer::new(
                DEFAULT_CONCURRENCY_LIMIT,
            ))
            .into_inner(),
    )
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
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    use crate::route::{
        DispatchStrategy, Handler, HandlerKind, QualName, ResponseType, RouteManifest, RoutePath,
    };
    use crate::with_py;

    fn make_route(kind: HandlerKind) -> BoundRoute {
        with_py(|py| BoundRoute {
            manifest: RouteManifest {
                kind,
                method: HttpMethod::Get,
                path: RoutePath::new("/test").unwrap(),
                handler_qualname: QualName::new("test.handler").unwrap(),
                params: Vec::new(),
                response_type: ResponseType::RawResponse,
                tags: Vec::new(),
                dependency_plan: None,
                status_code: 200,
                summary: None,
                description: None,
                include_in_schema: true,
                deprecated: false,
                operation_id: None,
                is_async_handler: true,
                dispatch_strategy: DispatchStrategy::default(),
            },
            handler: Handler::stub(py.None()),
            fastapi_app: None,
            direct_context: None,
        })
    }

    #[test]
    fn dispatch_for_asgi_bridge() {
        let route = make_route(HandlerKind::RequestResponse);
        let d = dispatch_for(&route);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AsgiBridgeDispatch"));
    }

    #[test]
    fn dispatch_for_direct() {
        let mut route = make_route(HandlerKind::RequestResponse);
        route.manifest.dispatch_strategy = DispatchStrategy::Direct;
        let d = dispatch_for(&route);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("DirectDispatch"));
    }

    #[test]
    fn dispatch_for_default_is_asgi_bridge() {
        // Routes without explicit dispatch_strategy should default to AsgiBridge.
        let route = make_route(HandlerKind::RequestResponse);
        assert_eq!(
            route.manifest.dispatch_strategy,
            DispatchStrategy::AsgiBridge
        );
        let d = dispatch_for(&route);
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

        let req = http::Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);

        let req = http::Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[test]
    fn register_ws_route_uses_get() {
        let ws_route = make_route(HandlerKind::WebSocket);
        let mut event_loop = crate::event_loop::EventLoop::start().unwrap();
        let server_addr = SocketAddr::from(([127, 0, 0, 1], 8080));
        let scope_interns = pyo3::Python::attach(asgi::ScopeInterns::new);
        let scope_interns = Arc::new(scope_interns);
        let scope_template = pyo3::Python::attach(|py| {
            context_pool::build_scope_template(py, &scope_interns, None, server_addr).unwrap()
        });
        let receive_template =
            pyo3::Python::attach(|py| context_pool::build_receive_template(py).unwrap());
        let (create_task, error_logger) = pyo3::Python::attach(|py| {
            let ct = event_loop
                .event_loop_ref()
                .getattr(py, "create_task")
                .unwrap();
            (ct, py.None())
        });
        let app_state = Arc::new(AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
            loop_handle: event_loop.handle().unwrap(),
            scope_interns,
            scope_template: Arc::new(scope_template),
            receive_template: Arc::new(receive_template),
            create_task,
            error_logger,
            scheduler_refs: None,
        });

        let _router = register_routes(Router::new(), vec![ws_route], &app_state, server_addr);
        event_loop.stop();
    }
}
