use axum::body::{to_bytes, Body};
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::SinkExt;
use tokio::select;
use tokio_stream::StreamExt;
use tokio_tungstenite::tungstenite::http::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tracing::{debug, warn};

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

// Header used by the Vite plugin to verify proxy requests
const APX_DEV_TOKEN_HEADER: &str = "x-apx-dev-token";

#[derive(Clone)]
pub struct ApiProxyState {
    pub client: reqwest::Client,
    pub host: String,
    pub port: u16,
}

#[derive(Clone)]
pub struct UiProxyState {
    pub client: reqwest::Client,
    pub host: String,
    pub port: u16,
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
pub fn api_router(backend_port: u16) -> Result<Router, String> {
    let state = ApiProxyState {
        client: build_proxy_client()?,
        host: "0.0.0.0".to_string(),
        port: backend_port,
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
        host: "localhost".to_string(),
        port: frontend_port,
        dev_token: dev_token.to_string(),
    };
    Ok(Router::new()
        .route("/", any(ui_proxy_handler))
        .route("/{*path}", any(ui_proxy_handler))
        .with_state(state))
}

async fn api_proxy_handler(State(state): State<ApiProxyState>, req: Request<Body>) -> Response {
    let original_uri = req.uri().clone();
    // Reconstruct path with /api prefix since nest strips it
    let path_and_query = format!(
        "/api{}",
        original_uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
    );
    proxy_request(req, state.client, state.host, state.port, path_and_query, None).await
}

async fn ui_proxy_handler(State(state): State<UiProxyState>, req: Request<Body>) -> Response {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    proxy_request(
        req,
        state.client,
        state.host,
        state.port,
        path_and_query,
        Some(state.dev_token.as_str()),
    )
    .await
}

async fn proxy_request(
    req: Request<Body>,
    client: reqwest::Client,
    host: String,
    target_port: u16,
    path_and_query: String,
    dev_token: Option<&str>,
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
    proxy_http(req, client, host, target_port, path_and_query, dev_token).await
}

async fn proxy_http(
    req: Request<Body>,
    client: reqwest::Client,
    host: String,
    target_port: u16,
    path_and_query: String,
    dev_token: Option<&str>,
) -> Response {
    let (parts, body) = req.into_parts();
    let url = format!("http://{host}:{target_port}{path_and_query}");
    let body_bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(error = %err, "Failed to read proxy request body.");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut builder = client.request(parts.method, url);
    for (name, value) in parts.headers.iter() {
        if is_hop_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Some(dev_token) = dev_token {
        builder = builder.header(APX_DEV_TOKEN_HEADER, dev_token);
    }
    let response = match builder.body(body_bytes).send().await {
        Ok(response) => response,
        Err(err) => {
            warn!(error = %err, "Proxy request failed.");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let status = response.status();
    let headers = response.headers().clone();
    let body = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(error = %err, "Failed to read proxy response body.");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if is_hop_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(body))
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
                        if let Some(mapped) = tungstenite_to_axum(message) {
                            if let Err(err) = downstream.send(mapped).await {
                                warn!(error = %err, "Failed to send websocket message downstream.");
                                break;
                            }
                        }
                    }
                    Some(Err(err)) => {
                        warn!(error = %err, "Upstream websocket error.");
                        break;
                    }
                    None => break,
                }
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
        TungsteniteMessage::Text(text) => {
            Some(Message::Text(Utf8Bytes::from(text.to_string())))
        }
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
    connection.to_ascii_lowercase().contains("upgrade")
        && upgrade.eq_ignore_ascii_case("websocket")
}

fn filter_headers(headers: HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (name, value) in headers.iter() {
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
