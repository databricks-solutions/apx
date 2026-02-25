use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::SinkExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::select;
use tokio_stream::StreamExt;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::http::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tracing::{debug, info, warn};

use apx_common::hosts::CLIENT_HOST;
use apx_databricks_sdk::DatabricksClient;

use crate::dev::token::DEV_TOKEN_HEADER;

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
const HOP_HEADERS: [&str; 8] = [
    "connection",
    "upgrade",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "host",
];

// Header used to forward OAuth access token to API
const ACCESS_TOKEN_HEADER: &str = "X-Forwarded-Access-Token";
// Header used to forward user identity to API
const FORWARDED_USER_HEADER: &str = "X-Forwarded-User";
/// Check if a request path should be logged (filters out Vite dev assets).
fn should_log_request(path: &str, is_ui: bool) -> bool {
    // Skip Vite dev server internal paths
    if path.starts_with("/@") {
        return false;
    }
    // Skip TanStack Router code splitting requests
    if is_ui && path.contains("?tsr-split") {
        return false;
    }
    // Skip common static assets served by Vite
    let lower = path.to_ascii_lowercase();
    // Get just the path part (before query string) for extension check
    let path_only = lower.split('?').next().unwrap_or(&lower);
    if path_only.ends_with(".js")
        || path_only.ends_with(".ts")
        || path_only.ends_with(".tsx")
        || path_only.ends_with(".jsx")
        || path_only.ends_with(".css")
        || path_only.ends_with(".map")
        || path_only.ends_with(".svg")
        || path_only.ends_with(".png")
        || path_only.ends_with(".jpg")
        || path_only.ends_with(".jpeg")
        || path_only.ends_with(".gif")
        || path_only.ends_with(".ico")
        || path_only.ends_with(".woff")
        || path_only.ends_with(".woff2")
        || path_only.ends_with(".ttf")
        || path_only.ends_with(".eot")
    {
        return false;
    }
    // Skip node_modules paths
    if path.contains("/node_modules/") {
        return false;
    }
    true
}

/// Manages OAuth token refresh for Databricks API proxy requests.
#[derive(Debug)]
pub struct TokenManager {
    client: Option<DatabricksClient>,
}

impl TokenManager {
    /// Create a new token manager with an optional Databricks client.
    pub fn new(client: Option<DatabricksClient>) -> Self {
        Self { client }
    }

    /// Retrieve a fresh OAuth access token, refreshing if necessary.
    pub async fn get_token_refreshing_if_needed(&self) -> Option<String> {
        let client = self.client.as_ref()?;
        match client.access_token().await {
            Ok(token) => Some(token),
            Err(err) => {
                warn!("Failed to get OAuth access token: {err}");
                None
            }
        }
    }
}

/// Shared state for the API reverse proxy.
#[derive(Clone, Debug)]
pub struct ApiProxyState {
    /// HTTP client used for proxied requests.
    pub client: reqwest::Client,
    /// Backend host address.
    pub host: String,
    /// Backend port.
    pub port: u16,
    /// Token manager for OAuth header injection.
    pub token_manager: Arc<TokenManager>,
    /// Pre-computed forwarded user header value.
    pub forwarded_user_header: Option<String>,
}

/// Shared state for the UI reverse proxy.
#[derive(Clone, Debug)]
pub struct UiProxyState {
    /// HTTP client used for proxied requests.
    pub client: reqwest::Client,
    /// Frontend host address.
    pub host: String,
    /// Frontend port.
    pub port: u16,
    /// Dev token for authenticating proxy requests.
    pub dev_token: String,
}

fn build_proxy_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|err| format!("Failed to build proxy HTTP client: {err}"))
}

/// Creates the API proxy router (nested at /api)
pub fn api_router(
    backend_port: u16,
    token_manager: Arc<TokenManager>,
    forwarded_user_header: Option<String>,
) -> Result<Router, String> {
    let state = ApiProxyState {
        client: build_proxy_client()?,
        host: CLIENT_HOST.to_string(),
        port: backend_port,
        token_manager,
        forwarded_user_header,
    };
    Ok(Router::new()
        .route("/", any(api_proxy_handler))
        .route("/{*path}", any(api_proxy_handler))
        .with_state(state))
}

/// Creates the UI proxy router (handles / and /*path)
pub fn ui_router(frontend_port: u16, dev_token: &str) -> Result<Router, String> {
    let state = UiProxyState {
        client: build_proxy_client()?,
        host: CLIENT_HOST.to_string(),
        port: frontend_port,
        dev_token: dev_token.to_string(),
    };
    Ok(Router::new()
        .route("/", any(ui_proxy_handler))
        .route("/{*path}", any(ui_proxy_handler))
        .with_state(state))
}

/// Creates the API utilities proxy router for FastAPI docs and OpenAPI schema
/// Routes: /docs, /redoc, /openapi.json - proxied directly to backend without /api prefix
pub fn api_utils_router(
    backend_port: u16,
    token_manager: Arc<TokenManager>,
    forwarded_user_header: Option<String>,
) -> Result<Router, String> {
    let state = ApiProxyState {
        client: build_proxy_client()?,
        host: CLIENT_HOST.to_string(),
        port: backend_port,
        token_manager,
        forwarded_user_header,
    };
    Ok(Router::new()
        .route("/docs", any(api_utils_proxy_handler))
        .route("/redoc", any(api_utils_proxy_handler))
        .route("/openapi.json", any(api_utils_proxy_handler))
        .with_state(state))
}

async fn api_proxy_handler(State(state): State<ApiProxyState>, req: Request<Body>) -> Response {
    let original_uri = req.uri().clone();
    // Reconstruct path with /api prefix since nest strips it
    let path_and_query = format!(
        "/api{}",
        original_uri.path_and_query().map_or("/", |pq| pq.as_str())
    );

    // Get OAuth access token for API requests (None if not available)
    let token = state.token_manager.get_token_refreshing_if_needed().await;
    proxy_request(
        req,
        state.client,
        state.host,
        state.port,
        path_and_query,
        None,
        token,
        state.forwarded_user_header.clone(),
        "api",
    )
    .await
}

async fn api_utils_proxy_handler(
    State(state): State<ApiProxyState>,
    req: Request<Body>,
) -> Response {
    // Pass through path directly without /api prefix (for /docs, /redoc, /openapi.json)
    let path_and_query = req
        .uri()
        .path_and_query()
        .map_or("/", |pq| pq.as_str())
        .to_string();

    // Get OAuth access token for API requests (None if not available)
    let token = state.token_manager.get_token_refreshing_if_needed().await;
    proxy_request(
        req,
        state.client,
        state.host,
        state.port,
        path_and_query,
        None,
        token,
        state.forwarded_user_header.clone(),
        "api",
    )
    .await
}

async fn ui_proxy_handler(State(state): State<UiProxyState>, req: Request<Body>) -> Response {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map_or("/", |pq| pq.as_str())
        .to_string();
    proxy_request(
        req,
        state.client,
        state.host,
        state.port,
        path_and_query,
        Some(state.dev_token.as_str()),
        None,
        None,
        "ui",
    )
    .await
}

// Reason: proxy forwarding inherently requires many context parameters
#[allow(clippy::too_many_arguments)]
async fn proxy_request(
    req: Request<Body>,
    client: reqwest::Client,
    host: String,
    target_port: u16,
    path_and_query: String,
    dev_token: Option<&str>,
    access_token: Option<String>,
    forwarded_user_header: Option<String>,
    target_name: &'static str,
) -> Response {
    if is_websocket_request(req.headers()) {
        let (mut parts, _body) = req.into_parts();
        let headers = parts.headers.clone();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(ws) => ws,
            Err(err) => return err.into_response(),
        };
        return ws.on_upgrade(move |socket| {
            proxy_websocket(socket, host, target_port, path_and_query, headers)
        });
    }
    proxy_http(
        req,
        client,
        host,
        target_port,
        path_and_query,
        dev_token,
        access_token,
        forwarded_user_header,
        target_name,
    )
    .await
}

// Reason: proxy forwarding inherently requires many context parameters
#[allow(clippy::too_many_arguments)]
async fn proxy_http(
    req: Request<Body>,
    client: reqwest::Client,
    host: String,
    target_port: u16,
    path_and_query: String,
    dev_token: Option<&str>,
    access_token: Option<String>,
    forwarded_user_header: Option<String>,
    target_name: &'static str,
) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let is_ui = target_name == "ui";
    let should_log = should_log_request(&path_and_query, is_ui);
    let start = Instant::now();

    if should_log {
        info!("~> {} {} {}", target_name, method, path_and_query,);
    }

    let url = format!("http://{host}:{target_port}{path_and_query}");
    let body_bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(error = %err, "Failed to read proxy request body.");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut builder = client.request(parts.method, url);
    for (name, value) in &parts.headers {
        if is_hop_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Some(dev_token) = dev_token {
        builder = builder.header(DEV_TOKEN_HEADER, dev_token);
    }
    if let Some(access_token) = access_token {
        builder = builder.header(ACCESS_TOKEN_HEADER, access_token);
    }
    if let Some(forwarded_user_header) = forwarded_user_header {
        builder = builder.header(FORWARDED_USER_HEADER, forwarded_user_header);
    }
    let response = match builder.body(body_bytes).send().await {
        Ok(response) => response,
        Err(err) => {
            let elapsed = start.elapsed().as_millis();
            if err.is_connect() {
                // Connection refused is expected during startup while upstream boots
                debug!(
                    target = target_name,
                    path = %path_and_query,
                    "Proxy request failed - upstream not ready yet."
                );
                debug!(
                    "<~ {} {} {} 502 [{}ms] (upstream not ready)",
                    target_name, method, path_and_query, elapsed
                );
            } else {
                warn!(
                    target = target_name,
                    host = %host,
                    port = target_port,
                    path = %path_and_query,
                    error = %err,
                    "Proxy request failed - could not connect to upstream server."
                );
                info!(
                    "<~ {} {} {} 502 [{}ms] (connection failed: {})",
                    target_name, method, path_and_query, elapsed, err
                );
            }
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = response.status();
    let headers = response.headers().clone();
    if should_log {
        let elapsed = start.elapsed().as_millis();
        info!(
            "<~ {} {} {} {} [{}ms]",
            target_name,
            method,
            path_and_query,
            status.as_u16(),
            elapsed
        );
    }

    let body_stream = response.bytes_stream();

    let mut builder = Response::builder().status(status);
    for (name, value) in &headers {
        if is_hop_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

async fn proxy_websocket(
    mut downstream: WebSocket,
    host: String,
    target_port: u16,
    path_and_query: String,
    headers: HeaderMap,
) {
    let ws_url = format!("ws://{host}:{target_port}{path_and_query}");
    let mut request = match WsRequest::builder().uri(ws_url).body(()) {
        Ok(request) => request,
        Err(err) => {
            warn!(error = %err, "Failed to build websocket request.");
            return;
        }
    };
    *request.headers_mut() = filter_headers(headers);
    let upstream = match tokio_tungstenite::connect_async(request).await {
        Ok((stream, _)) => stream,
        Err(err) => {
            warn!(error = %err, "Failed to connect to upstream websocket.");
            return;
        }
    };

    let mut upstream = upstream;
    let idle_timeout = Duration::from_secs(300); // 5 minutes
    loop {
        select! {
            downstream_msg = downstream.recv() => {
                match downstream_msg {
                    Some(Ok(message)) => {
                        debug!("Proxy websocket downstream message.");
                        let mapped = axum_to_tungstenite(message);
                        if let Err(err) = upstream.send(mapped).await {
                            warn!(error = %err, "Failed to send websocket message upstream.");
                            break;
                        }
                    }
                    Some(Err(err)) => {
                        warn!(error = %err, "Downstream websocket error.");
                        break;
                    }
                    None => break,
                }
            }
            upstream_msg = upstream.next() => {
                match upstream_msg {
                    Some(Ok(message)) => {
                        debug!("Proxy websocket upstream message.");
                        if let Some(mapped) = tungstenite_to_axum(message)
                            && let Err(err) = downstream.send(mapped).await
                        {
                            warn!(error = %err, "Failed to send websocket message downstream.");
                            break;
                        }
                    }
                    Some(Err(err)) => {
                        warn!(error = %err, "Upstream websocket error.");
                        break;
                    }
                    None => break,
                }
            }
            () = tokio::time::sleep(idle_timeout) => {
                debug!("WebSocket idle timeout, closing proxy connection.");
                break;
            }
        }
    }
}

fn axum_to_tungstenite(message: Message) -> TungsteniteMessage {
    match message {
        Message::Text(text) => TungsteniteMessage::Text(text.as_str().to_string().into()),
        Message::Binary(binary) => TungsteniteMessage::Binary(binary),
        Message::Ping(ping) => TungsteniteMessage::Ping(ping),
        Message::Pong(pong) => TungsteniteMessage::Pong(pong),
        Message::Close(Some(close)) => TungsteniteMessage::Close(Some(TungsteniteCloseFrame {
            code: CloseCode::from(close.code),
            reason: close.reason.as_str().to_string().into(),
        })),
        Message::Close(None) => TungsteniteMessage::Close(None),
    }
}

fn tungstenite_to_axum(message: TungsteniteMessage) -> Option<Message> {
    match message {
        TungsteniteMessage::Text(text) => Some(Message::Text(Utf8Bytes::from(text.to_string()))),
        TungsteniteMessage::Binary(binary) => Some(Message::Binary(binary)),
        TungsteniteMessage::Ping(ping) => Some(Message::Ping(ping)),
        TungsteniteMessage::Pong(pong) => Some(Message::Pong(pong)),
        TungsteniteMessage::Close(Some(close)) => Some(Message::Close(Some(CloseFrame {
            code: close.code.into(),
            reason: Utf8Bytes::from(close.reason.to_string()),
        }))),
        TungsteniteMessage::Close(None) => Some(Message::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}

fn is_websocket_request(headers: &HeaderMap) -> bool {
    let connection = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    connection.to_ascii_lowercase().contains("upgrade") && upgrade.eq_ignore_ascii_case("websocket")
}

fn filter_headers(headers: HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (name, value) in &headers {
        if is_hop_header(name.as_str()) {
            continue;
        }
        filtered.append(name, value.clone());
    }
    filtered
}

fn is_hop_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HOP_HEADERS.iter().any(|header| *header == lower)
}
