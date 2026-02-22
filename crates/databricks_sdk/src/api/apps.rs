use std::collections::VecDeque;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::debug;

use crate::client::DatabricksClient;
use crate::error::{DatabricksError, Result};

// ---------------------------------------------------------------------------
// REST types for GET /api/2.0/apps/{name}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct App {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub compute_status: Option<ComputeStatus>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComputeState {
    Active,
    Starting,
    Stopping,
    Stopped,
    Deleting,
    Error,
    #[serde(other)]
    Unknown,
}

impl ComputeState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Deleting | Self::Error)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputeStatus {
    pub state: ComputeState,
}

// ---------------------------------------------------------------------------
// WebSocket log entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub source: String,
    pub timestamp: f64,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Input parameters
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AppLogsArgs<'a> {
    pub app_name: &'a str,
    pub tail_lines: usize,
    pub search: Option<&'a str>,
    pub sources: Option<&'a [String]>,
    pub timeout: Duration,
    pub idle_timeout: Option<Duration>,
}

// ---------------------------------------------------------------------------
// AppsApi
// ---------------------------------------------------------------------------

pub struct AppsApi<'a> {
    client: &'a DatabricksClient,
}

impl<'a> AppsApi<'a> {
    pub(crate) fn new(client: &'a DatabricksClient) -> Self {
        Self { client }
    }

    /// Fetch app metadata via REST API.
    pub async fn get(&self, name: &str) -> Result<App> {
        self.client.get(&format!("/api/2.0/apps/{name}")).await
    }

    /// Fetch app logs via WebSocket.
    pub async fn logs(&self, args: &AppLogsArgs<'_>) -> Result<Vec<LogEntry>> {
        let app = self.get(args.app_name).await?;
        let app_url = validate_app(&app)?;
        let ws_url = build_ws_url(app_url)?;
        let token = self.client.access_token().await?;
        let origin = extract_origin(app_url)?;

        debug!(ws_url = %ws_url, "Connecting to app log stream");
        let stream = connect_ws(&ws_url, &token, &origin).await?;
        collect_logs(stream, args).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_app(app: &App) -> Result<&str> {
    if let Some(ref cs) = app.compute_status
        && cs.state.is_terminal()
    {
        return Err(DatabricksError::Validation(format!(
            "App '{}' is in state {:?} and cannot produce logs",
            app.name, cs.state
        )));
    }

    app.url.as_deref().filter(|u| !u.is_empty()).ok_or_else(|| {
        DatabricksError::Validation(format!(
            "App '{}' has no URL — it may not be deployed yet",
            app.name
        ))
    })
}

fn build_ws_url(app_url: &str) -> Result<String> {
    let mut parsed = url::Url::parse(app_url)
        .map_err(|e| DatabricksError::WebSocket(format!("Invalid app URL: {e}")))?;

    let ws_scheme = match parsed.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(DatabricksError::WebSocket(format!(
                "Unexpected app URL scheme: {other}"
            )));
        }
    };

    parsed
        .set_scheme(ws_scheme)
        .map_err(|()| DatabricksError::WebSocket("Failed to set WS scheme".to_string()))?;

    // Append /logz/stream to existing path
    let path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(&format!("{path}/logz/stream"));

    Ok(parsed.to_string())
}

fn extract_origin(app_url: &str) -> Result<String> {
    let parsed = url::Url::parse(app_url)
        .map_err(|e| DatabricksError::WebSocket(format!("Invalid app URL: {e}")))?;

    match parsed.origin() {
        url::Origin::Tuple(scheme, host, port) => {
            let default_port = match scheme.as_str() {
                "https" | "wss" => 443,
                "http" | "ws" => 80,
                _ => 0,
            };
            if port == default_port {
                Ok(format!("{scheme}://{host}"))
            } else {
                Ok(format!("{scheme}://{host}:{port}"))
            }
        }
        url::Origin::Opaque(_) => Err(DatabricksError::WebSocket(
            "App URL has opaque origin".to_string(),
        )),
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_ws(url: &str, token: &str, origin: &str) -> Result<WsStream> {
    let mut request = url
        .into_client_request()
        .map_err(|e| DatabricksError::WebSocket(format!("Failed to build WS request: {e}")))?;

    let headers = request.headers_mut();
    headers.insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|e| DatabricksError::WebSocket(format!("Invalid auth header: {e}")))?,
    );
    headers.insert(
        "Origin",
        origin
            .parse()
            .map_err(|e| DatabricksError::WebSocket(format!("Invalid origin header: {e}")))?,
    );

    let (stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| DatabricksError::WebSocket(format!("WebSocket connection failed: {e}")))?;

    Ok(stream)
}

/// The heartbeat frame is a single null byte.
const HEARTBEAT: &[u8] = &[0x00];

/// Default idle timeout: if no text frame arrives within this window after
/// we've already received at least one, we assume the backlog is done.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

async fn collect_logs(mut stream: WsStream, args: &AppLogsArgs<'_>) -> Result<Vec<LogEntry>> {
    // Send search term as the first text frame.
    let search_term = args.search.unwrap_or("");
    stream
        .send(Message::Text(search_term.to_string().into()))
        .await
        .map_err(|e| DatabricksError::WebSocket(format!("Failed to send search term: {e}")))?;

    let mut buffer: VecDeque<LogEntry> = VecDeque::with_capacity(args.tail_lines);

    let result =
        tokio::time::timeout(args.timeout, read_frames(&mut stream, args, &mut buffer)).await;

    // Close the stream gracefully regardless of outcome.
    let _ = stream.close(None).await;

    match result {
        Ok(inner) => inner?,
        Err(_) => debug!("Log stream timed out after {:?}", args.timeout),
    }

    Ok(buffer.into())
}

async fn read_frames(
    stream: &mut WsStream,
    args: &AppLogsArgs<'_>,
    buffer: &mut VecDeque<LogEntry>,
) -> Result<()> {
    let idle = args.idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT);
    let mut received_any = false;

    loop {
        let next = tokio::time::timeout(idle, stream.next()).await;

        match next {
            Ok(Some(msg)) => {
                let msg = msg.map_err(|e| {
                    DatabricksError::WebSocket(format!("Failed to read frame: {e}"))
                })?;

                match msg {
                    Message::Text(text) => {
                        received_any = true;
                        parse_and_buffer(text.as_ref(), args, buffer);
                    }
                    Message::Binary(data) if data.as_ref() == HEARTBEAT => continue,
                    Message::Close(_) => break,
                    _ => continue,
                }
            }
            Ok(None) => break, // stream ended
            Err(_) => {
                // Idle timeout elapsed
                if received_any {
                    debug!("Idle timeout reached after receiving logs, assuming backlog complete");
                    break;
                }
                // Haven't received any logs yet — keep waiting (outer timeout guards)
                continue;
            }
        }
    }
    Ok(())
}

fn parse_and_buffer(text: &str, args: &AppLogsArgs<'_>, buffer: &mut VecDeque<LogEntry>) {
    let entry: LogEntry = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            debug!(error = %e, "Skipping unparseable log frame");
            return;
        }
    };

    if let Some(sources) = args.sources
        && !sources
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&entry.source))
    {
        return;
    }

    if buffer.len() >= args.tail_lines {
        buffer.pop_front();
    }
    buffer.push_back(entry);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn build_ws_url_https() {
        let result = build_ws_url("https://my-app.databricks.app").unwrap();
        assert_eq!(result, "wss://my-app.databricks.app/logz/stream");
    }

    #[test]
    fn build_ws_url_http() {
        let result = build_ws_url("http://localhost:8080").unwrap();
        assert_eq!(result, "ws://localhost:8080/logz/stream");
    }

    #[test]
    fn build_ws_url_trailing_slash() {
        let result = build_ws_url("https://my-app.databricks.app/").unwrap();
        assert_eq!(result, "wss://my-app.databricks.app/logz/stream");
    }

    #[test]
    fn build_ws_url_bad_scheme() {
        let result = build_ws_url("ftp://something");
        assert!(result.is_err());
    }

    #[test]
    fn extract_origin_basic() {
        let result = extract_origin("https://my-app.databricks.app/some/path").unwrap();
        assert_eq!(result, "https://my-app.databricks.app");
    }

    #[test]
    fn extract_origin_with_port() {
        let result = extract_origin("http://localhost:8080/path").unwrap();
        assert_eq!(result, "http://localhost:8080");
    }

    #[test]
    fn tail_buffer_respects_capacity() {
        let args = AppLogsArgs {
            app_name: "test",
            tail_lines: 3,
            search: None,
            sources: None,
            timeout: Duration::from_secs(5),
            idle_timeout: None,
        };
        let mut buffer = VecDeque::with_capacity(3);

        for i in 0..5 {
            let json = format!(r#"{{"source":"APP","timestamp":{i}.0,"message":"line {i}"}}"#);
            parse_and_buffer(&json, &args, &mut buffer);
        }

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer[0].message, "line 2");
        assert_eq!(buffer[1].message, "line 3");
        assert_eq!(buffer[2].message, "line 4");
    }

    #[test]
    fn source_filtering() {
        let sources = vec!["APP".to_string()];
        let args = AppLogsArgs {
            app_name: "test",
            tail_lines: 100,
            search: None,
            sources: Some(&sources),
            timeout: Duration::from_secs(5),
            idle_timeout: None,
        };
        let mut buffer = VecDeque::new();

        parse_and_buffer(
            r#"{"source":"APP","timestamp":1.0,"message":"app log"}"#,
            &args,
            &mut buffer,
        );
        parse_and_buffer(
            r#"{"source":"SYSTEM","timestamp":2.0,"message":"sys log"}"#,
            &args,
            &mut buffer,
        );

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].source, "APP");
    }

    #[test]
    fn source_filtering_case_insensitive() {
        let sources = vec!["app".to_string()];
        let args = AppLogsArgs {
            app_name: "test",
            tail_lines: 100,
            search: None,
            sources: Some(&sources),
            timeout: Duration::from_secs(5),
            idle_timeout: None,
        };
        let mut buffer = VecDeque::new();

        parse_and_buffer(
            r#"{"source":"APP","timestamp":1.0,"message":"app log"}"#,
            &args,
            &mut buffer,
        );

        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn validate_app_stopped() {
        let app = App {
            name: "test-app".to_string(),
            url: Some("https://test.databricks.app".to_string()),
            compute_status: Some(ComputeStatus {
                state: ComputeState::Stopped,
            }),
        };
        assert!(validate_app(&app).is_err());
    }

    #[test]
    fn validate_app_no_url() {
        let app = App {
            name: "test-app".to_string(),
            url: None,
            compute_status: Some(ComputeStatus {
                state: ComputeState::Active,
            }),
        };
        assert!(validate_app(&app).is_err());
    }

    #[test]
    fn validate_app_active_with_url() {
        let app = App {
            name: "test-app".to_string(),
            url: Some("https://test.databricks.app".to_string()),
            compute_status: Some(ComputeStatus {
                state: ComputeState::Active,
            }),
        };
        let result = validate_app(&app).unwrap();
        assert_eq!(result, "https://test.databricks.app");
    }

    #[test]
    fn parse_and_buffer_ignores_invalid_json() {
        let args = AppLogsArgs {
            app_name: "test",
            tail_lines: 100,
            search: None,
            sources: None,
            timeout: Duration::from_secs(5),
            idle_timeout: None,
        };
        let mut buffer = VecDeque::new();

        parse_and_buffer("not json", &args, &mut buffer);
        assert!(buffer.is_empty());
    }

    // ----- New API-level tests -----

    #[test]
    fn deserialize_app_active_full() {
        let json = r#"{
            "name": "my-app",
            "url": "https://my-app.databricks.app",
            "compute_status": { "state": "ACTIVE" }
        }"#;
        let app: App = serde_json::from_str(json).unwrap();
        assert_eq!(app.name, "my-app");
        assert_eq!(app.url.as_deref(), Some("https://my-app.databricks.app"));
        assert_eq!(app.compute_status.unwrap().state, ComputeState::Active);
    }

    #[test]
    fn deserialize_app_minimal() {
        let json = r#"{ "name": "bare-app" }"#;
        let app: App = serde_json::from_str(json).unwrap();
        assert_eq!(app.name, "bare-app");
        assert!(app.url.is_none());
        assert!(app.compute_status.is_none());
    }

    #[test]
    fn deserialize_app_unknown_state() {
        let json = r#"{
            "name": "future-app",
            "compute_status": { "state": "SOME_FUTURE_STATE" }
        }"#;
        let app: App = serde_json::from_str(json).unwrap();
        assert_eq!(app.compute_status.unwrap().state, ComputeState::Unknown);
    }

    #[test]
    fn deserialize_log_entry_valid() {
        let json = r#"{"source":"APP","timestamp":1700000000.123,"message":"hello world"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.source, "APP");
        assert!((entry.timestamp - 1700000000.123).abs() < f64::EPSILON);
        assert_eq!(entry.message, "hello world");
    }

    #[test]
    fn deserialize_log_entry_extra_fields() {
        let json = r#"{
            "source": "APP",
            "timestamp": 1.0,
            "message": "msg",
            "extra_field": "ignored",
            "another": 42
        }"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.message, "msg");
    }

    #[test]
    fn deserialize_log_entry_missing_field() {
        let json = r#"{"source":"APP","timestamp":1.0}"#;
        let result: std::result::Result<LogEntry, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn validate_app_all_terminal_states() {
        for state in [
            ComputeState::Stopped,
            ComputeState::Deleting,
            ComputeState::Error,
        ] {
            let app = App {
                name: "test-app".to_string(),
                url: Some("https://test.databricks.app".to_string()),
                compute_status: Some(ComputeStatus { state }),
            };
            assert!(
                validate_app(&app).is_err(),
                "Expected error for terminal state {:?}",
                app.compute_status.as_ref().unwrap().state
            );
        }
    }

    #[test]
    fn validate_app_non_terminal_states() {
        for state in [
            ComputeState::Active,
            ComputeState::Starting,
            ComputeState::Unknown,
        ] {
            let app = App {
                name: "test-app".to_string(),
                url: Some("https://test.databricks.app".to_string()),
                compute_status: Some(ComputeStatus { state }),
            };
            assert!(
                validate_app(&app).is_ok(),
                "Expected Ok for non-terminal state {:?}",
                app.compute_status.as_ref().unwrap().state
            );
        }
    }

    #[test]
    fn parse_and_buffer_mixed_scenario() {
        let sources = vec!["APP".to_string()];
        let args = AppLogsArgs {
            app_name: "test",
            tail_lines: 100,
            search: None,
            sources: Some(&sources),
            timeout: Duration::from_secs(5),
            idle_timeout: None,
        };
        let mut buffer = VecDeque::new();

        // 1. Valid APP entry
        parse_and_buffer(
            r#"{"source":"APP","timestamp":1.0,"message":"first"}"#,
            &args,
            &mut buffer,
        );
        // 2. Bad JSON
        parse_and_buffer("not json at all", &args, &mut buffer);
        // 3. Filtered SYSTEM entry
        parse_and_buffer(
            r#"{"source":"SYSTEM","timestamp":2.0,"message":"sys"}"#,
            &args,
            &mut buffer,
        );
        // 4. Valid APP entry
        parse_and_buffer(
            r#"{"source":"APP","timestamp":3.0,"message":"second"}"#,
            &args,
            &mut buffer,
        );
        // 5. Missing required field
        parse_and_buffer(r#"{"source":"APP","timestamp":4.0}"#, &args, &mut buffer);

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer[0].message, "first");
        assert_eq!(buffer[1].message, "second");
    }
}
