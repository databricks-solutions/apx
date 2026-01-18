use crate::mcp::core::{McpServer, ToolResult};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppContext {
    pub app_dir: PathBuf,
}

pub fn build_server(ctx: AppContext) -> McpServer<AppContext> {
    McpServer::new(ctx)
        .resource(
            "apx://info",
            "apx-info",
            "Information about apx toolkit",
            "text/plain",
            apx_info_resource,
        )
        .tool(
            "start",
            "Start development server and return the URL",
            start_tool,
        )
}

// --- Resources ---

async fn apx_info_resource(_ctx: Arc<AppContext>) -> Result<String, String> {
    Ok(APX_INFO_CONTENT.to_string())
}

// --- Tools ---

#[derive(Deserialize, schemars::JsonSchema)]
pub struct StartArgs {}

async fn start_tool(ctx: Arc<AppContext>, _args: StartArgs) -> ToolResult {
    use crate::cli::dev::start::start_dev_server;
    use crate::dev::common::CLIENT_HOST;

    let app_dir = &ctx.app_dir;

    match start_dev_server(app_dir).await {
        Ok(port) => ToolResult::success(format!(
            "Dev server started at http://{}:{}",
            CLIENT_HOST, port
        )),
        Err(e) => ToolResult::error(e),
    }
}

const APX_INFO_CONTENT: &str = r#"# apx - Toolkit for Building Databricks Apps

🚀 **apx** is the toolkit for building Databricks Apps ⚡**

apx bundles together a set of tools and libraries to help you with the complete app development lifecycle: develop, build and deploy.

## Overview

The main idea of apx is to provide convenient, fast and AI-friendly development experience for building modern full-stack applications.

## Technology Stack

- **Backend**: Python + FastAPI + Pydantic
- **Frontend**: React + TypeScript + shadcn/ui
- **Build Tools**: uv (Python), bun (JavaScript/TypeScript)
- **Code Generation**: orval (OpenAPI client generation)

## What This MCP Server Provides

This MCP server gives you access to development server management tools:
- **start**: Start development servers (frontend, backend, OpenAPI watcher)
- **restart**: Restart all development servers
- **stop**: Stop all development servers  
- **status**: Get dev server status and URL
- **refresh_openapi**: Trigger OpenAPI schema and api.ts regeneration
- **list_routes**: List available backend API routes
- **call_route**: Call a backend route through the dev server proxy
- **get_metadata**: Get project metadata from pyproject.toml

Databricks SDK documentation tools:
- **search_databricks_sdk**: Search SDK methods by natural language query
- **get_method_spec**: Get detailed specification for a specific SDK method

Resources:
- **apx://info**: Information about apx toolkit
- **apx://backend/openapi**: Backend OpenAPI schema
- **apx://openapi/status**: OpenAPI regeneration timestamps

Use these tools to interact with your apx project during development."#;
