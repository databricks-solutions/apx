use futures_util::future::BoxFuture;
use schemars::schema_for;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::broadcast;

// JSON-RPC constants
pub const JSONRPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const PARSE_ERROR: i32 = -32700;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;

// JSON-RPC 2.0 Types
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err("Invalid JSON-RPC version, expected 2.0");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
}

impl JsonRpcNotification {
    pub fn is_valid(&self) -> bool {
        self.jsonrpc == JSONRPC_VERSION
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

impl JsonRpcResponse {
    pub fn new(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("JsonRpcResponse serialization should never fail")
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: ErrorObject,
}

impl JsonRpcError {
    pub fn new(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            error: ErrorObject {
                code,
                message: message.into(),
            },
        }
    }

    pub fn parse_error() -> Self {
        Self::new(Value::Null, PARSE_ERROR, "Parse error")
    }

    pub fn method_not_found(id: Value, method: &str) -> Self {
        Self::new(id, METHOD_NOT_FOUND, format!("Method not found: {}", method))
    }

    pub fn invalid_params(id: Value, message: impl Into<String>) -> Self {
        Self::new(id, INVALID_PARAMS, message)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("JsonRpcError serialization should never fail")
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

// MCP Protocol Types
#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub resources: ResourceCapabilities,
    pub tools: ToolCapabilities,
}

#[derive(Debug, Serialize)]
pub struct ResourceCapabilities {}

#[derive(Debug, Serialize)]
pub struct ToolCapabilities {}

#[derive(Debug, Serialize, Clone)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub text: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(text: String) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: false,
        }
    }

    pub fn error(text: String) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: true,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl ContentBlock {
    pub fn text(text: String) -> Self {
        Self {
            content_type: "text".to_string(),
            text,
        }
    }
}

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub type ToolHandler<C> =
    Arc<dyn Fn(Arc<C>, Value) -> BoxFuture<'static, ToolResult> + Send + Sync>;
pub type ResourceHandler<C> =
    Arc<dyn Fn(Arc<C>) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

pub struct ToolDef<C> {
    pub description: String,
    pub input_schema: Value,
    pub handler: ToolHandler<C>,
}

pub struct ResourceDef<C> {
    pub name: String,
    pub description: String,
    pub mime_type: String,
    pub handler: ResourceHandler<C>,
}

pub struct McpServer<C> {
    pub ctx: Arc<C>,
    pub tools: HashMap<String, ToolDef<C>>,
    pub resources: HashMap<String, ResourceDef<C>>,
}

impl<C: Send + Sync + 'static> McpServer<C> {
    pub fn new(ctx: C) -> Self {
        Self {
            ctx: Arc::new(ctx),
            tools: HashMap::new(),
            resources: HashMap::new(),
        }
    }

    pub fn tool<A, F, Fut>(mut self, name: &str, description: &str, handler: F) -> Self
    where
        A: DeserializeOwned + schemars::JsonSchema + Send + 'static,
        F: Fn(Arc<C>, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        let schema = schema_for!(A);
        let input_schema =
            serde_json::to_value(&schema).expect("Tool input schema should serialize");
        let handler = Arc::new(handler);
        let handler = Arc::new(move |ctx: Arc<C>, arguments: Value| -> BoxFuture<'static, ToolResult> {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let parsed: Result<A, _> = serde_json::from_value(arguments);
                match parsed {
                    Ok(args) => handler(ctx, args).await,
                    Err(err) => ToolResult::error(format!("Invalid tool arguments: {}", err)),
                }
            })
        });

        self.tools.insert(
            name.to_string(),
            ToolDef {
                description: description.to_string(),
                input_schema,
                handler,
            },
        );
        self
    }

    pub fn resource<F, Fut>(
        mut self,
        uri: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) -> Self
    where
        F: Fn(Arc<C>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let handler =
            Arc::new(move |ctx: Arc<C>| -> BoxFuture<'static, Result<String, String>> {
                let handler = Arc::clone(&handler);
                Box::pin(async move { handler(ctx).await })
            });
        self.resources.insert(
            uri.to_string(),
            ResourceDef {
                name: name.to_string(),
                description: description.to_string(),
                mime_type: mime_type.to_string(),
                handler,
            },
        );
        self
    }

    pub fn list_tools(&self) -> Vec<Tool> {
        self.tools
            .iter()
            .map(|(name, def)| Tool {
                name: name.clone(),
                description: def.description.clone(),
                input_schema: def.input_schema.clone(),
            })
            .collect()
    }

    pub fn list_resources(&self) -> Vec<Resource> {
        self.resources
            .iter()
            .map(|(uri, def)| Resource {
                uri: uri.clone(),
                name: def.name.clone(),
                description: def.description.clone(),
                mime_type: def.mime_type.clone(),
            })
            .collect()
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult, String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("Unknown tool: {}", name))?;
        Ok((tool.handler)(Arc::clone(&self.ctx), arguments).await)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ResourceContent, String> {
        let resource = self
            .resources
            .get(uri)
            .ok_or_else(|| format!("Resource not found: {}", uri))?;
        let text = (resource.handler)(Arc::clone(&self.ctx)).await?;
        Ok(ResourceContent {
            uri: uri.to_string(),
            mime_type: resource.mime_type.clone(),
            text,
        })
    }

    pub fn initialize_result(
        &self,
        protocol_version: &str,
        server_name: &str,
        server_version: &str,
    ) -> Value {
        json!({
            "protocolVersion": protocol_version,
            "capabilities": ServerCapabilities {
                resources: ResourceCapabilities {},
                tools: ToolCapabilities {},
            },
            "serverInfo": ServerInfo {
                name: server_name.to_string(),
                version: server_version.to_string(),
            }
        })
    }

    pub async fn run_stdio(self, shutdown_tx: broadcast::Sender<()>) -> Result<(), String> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF - signal shutdown to background tasks
                    let _ = shutdown_tx.send(());
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    if let Some(response) = self.handle_message(trimmed).await {
                        self.write_response(&mut stdout, &response).await?;
                    }
                }
                Err(e) => {
                    // Error - signal shutdown to background tasks
                    let _ = shutdown_tx.send(());
                    return Err(format!("Error reading from stdin: {}", e));
                }
            }
        }

        Ok(())
    }

    async fn write_response(
        &self,
        stdout: &mut tokio::io::Stdout,
        response: &str,
    ) -> Result<(), String> {
        stdout
            .write_all(response.as_bytes())
            .await
            .map_err(|e| format!("Failed to write response: {}", e))?;
        stdout
            .write_all(b"\n")
            .await
            .map_err(|e| format!("Failed to write newline: {}", e))?;
        stdout
            .flush()
            .await
            .map_err(|e| format!("Failed to flush stdout: {}", e))?;
        Ok(())
    }

    async fn handle_message(&self, msg: &str) -> Option<String> {
        if let Ok(notification) = serde_json::from_str::<JsonRpcNotification>(msg) {
            if notification.is_valid() && notification.method == "notifications/initialized" {
                return None;
            }
        }

        let request: JsonRpcRequest = match serde_json::from_str(msg) {
            Ok(req) => req,
            Err(_) => return Some(JsonRpcError::parse_error().to_json()),
        };

        if let Err(_) = request.validate() {
            return Some(JsonRpcError::parse_error().to_json());
        }

        let id = request.id.unwrap_or(Value::Null);
        let params = request.params.unwrap_or(Value::Null);

        let response = match request.method.as_str() {
            "initialize" => JsonRpcResponse::new(
                id,
                self.initialize_result(PROTOCOL_VERSION, "apx", env!("CARGO_PKG_VERSION")),
            )
            .to_json(),
            "resources/list" => {
                JsonRpcResponse::new(id, json!({ "resources": self.list_resources() })).to_json()
            }
            "resources/read" => self.handle_resources_read(id, params).await,
            "tools/list" => {
                JsonRpcResponse::new(id, json!({ "tools": self.list_tools() })).to_json()
            }
            "tools/call" => self.handle_tools_call(id, params).await,
            method => JsonRpcError::method_not_found(id, method).to_json(),
        };

        Some(response)
    }

    async fn handle_resources_read(&self, id: Value, params: Value) -> String {
        let uri = match params.get("uri").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return JsonRpcError::invalid_params(id, "Missing 'uri' parameter").to_json(),
        };

        match self.read_resource(uri).await {
            Ok(content) => JsonRpcResponse::new(id, json!({ "contents": vec![content] })).to_json(),
            Err(err) => JsonRpcError::invalid_params(id, err).to_json(),
        }
    }

    async fn handle_tools_call(&self, id: Value, params: Value) -> String {
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return JsonRpcError::invalid_params(id, "Missing 'name' parameter").to_json(),
        };

        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

        match self.call_tool(tool_name, arguments).await {
            Ok(result) => JsonRpcResponse::new(
                id,
                serde_json::to_value(result).expect("ToolResult serialization should never fail"),
            )
            .to_json(),
            Err(err) => JsonRpcError::invalid_params(id, err).to_json(),
        }
    }
}
