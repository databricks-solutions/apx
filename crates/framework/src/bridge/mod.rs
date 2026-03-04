//! Axum ↔ Python handler bridge.
//!
//! Wires bound routes into the axum router and delegates request handling
//! to the appropriate [`dispatch::HandlerDispatch`] implementation.

pub mod context;
pub mod dispatch;

use crate::route::{BoundRoute, HandlerKind, HttpMethod};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use dispatch::{AppState, HandlerDispatch, RequestResponseDispatch};
use std::collections::HashSet;
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
}

/// The axum handler function. Delegates to the dispatch trait.
///
/// Path params are extracted via axum's `RawPathParams` (percent-decoded).
async fn python_handler(
    axum::extract::State(state): axum::extract::State<HandlerState>,
    raw_params: axum::extract::RawPathParams,
    request: axum::extract::Request,
) -> Result<axum::response::Response, crate::error::AppError> {
    let path_params: Vec<(String, String)> = raw_params
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();

    state
        .dispatch
        .handle(
            Arc::clone(&state.route),
            Arc::clone(&state.app_state),
            path_params,
            request,
        )
        .await
}

/// Select the dispatch impl for a given handler kind.
fn dispatch_for(kind: HandlerKind) -> Arc<dyn HandlerDispatch> {
    match kind {
        // TODO(phase-3): SSE and WebSocket dispatch implementations
        HandlerKind::RequestResponse | HandlerKind::SSE | HandlerKind::WebSocket => {
            Arc::new(RequestResponseDispatch)
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
) -> Router {
    for route in routes {
        let dispatch = dispatch_for(route.manifest.kind);
        let method = route.manifest.method;
        let path = route.manifest.path.as_str().to_owned();
        let state = HandlerState {
            route: Arc::new(route),
            app_state: Arc::clone(app_state),
            dispatch,
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
pub fn build_router(routes: Vec<BoundRoute>, app_state: Arc<AppState>) -> Router {
    let user_paths: HashSet<&str> = routes.iter().map(|r| r.manifest.path.as_str()).collect();
    let router = register_health_probes(Router::new(), &user_paths);
    register_routes(router, routes, &app_state)
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
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    #[test]
    fn dispatch_for_request_response() {
        let d = dispatch_for(HandlerKind::RequestResponse);
        // Just verify it returns an Arc (coverage for the match arm)
        let _ = format!("{:?}", Arc::as_ptr(&d));
    }

    #[test]
    fn dispatch_for_sse() {
        let d = dispatch_for(HandlerKind::SSE);
        let _ = format!("{:?}", Arc::as_ptr(&d));
    }

    #[test]
    fn dispatch_for_websocket() {
        let d = dispatch_for(HandlerKind::WebSocket);
        let _ = format!("{:?}", Arc::as_ptr(&d));
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
}
