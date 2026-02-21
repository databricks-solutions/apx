use crate::server::ApxServer;
use crate::tools::{AppPathArgs, ToolResultExt};
use crate::validation::validate_app_path;
use rmcp::model::*;
use rmcp::schemars;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRouteInfoArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Operation ID from the OpenAPI schema (e.g., "listItems", "createItem")
    pub operation_id: String,
}

#[derive(Serialize, Clone)]
pub(crate) struct ParamInfo {
    name: String,
    location: String,
    param_type: String,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct RouteInfo {
    pub(crate) id: String,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) description: String,
    pub(crate) hook_name: String,
    pub(crate) parameters: Vec<ParamInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) request_body_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_schema: Option<Value>,
}

/// Single-level `$ref` resolution against `#/components/schemas/*`.
fn resolve_schema_ref<'a>(schema: &'a Value, components: Option<&'a Value>) -> &'a Value {
    if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str())
        && let Some(schema_name) = ref_str.strip_prefix("#/components/schemas/")
        && let Some(resolved) = components
            .and_then(|c| c.get("schemas"))
            .and_then(|s| s.get(schema_name))
    {
        return resolved;
    }
    schema
}

/// Extract parameters from an OpenAPI operation, merging path-level and operation-level params.
/// Operation-level params take precedence on name+location collision.
fn extract_parameters(operation: &Value, path_item: &Value) -> Vec<ParamInfo> {
    let mut params: Vec<ParamInfo> = Vec::new();

    // Collect path-level params first
    if let Some(path_params) = path_item.get("parameters").and_then(|p| p.as_array()) {
        for p in path_params {
            if let Some(info) = parse_single_param(p) {
                params.push(info);
            }
        }
    }

    // Collect operation-level params, overriding path-level on name+location match
    if let Some(op_params) = operation.get("parameters").and_then(|p| p.as_array()) {
        for p in op_params {
            if let Some(info) = parse_single_param(p) {
                // Remove any existing param with same name+location
                params.retain(|existing| {
                    !(existing.name == info.name && existing.location == info.location)
                });
                params.push(info);
            }
        }
    }

    params
}

fn parse_single_param(param: &Value) -> Option<ParamInfo> {
    let name = param.get("name").and_then(|v| v.as_str())?.to_string();
    let location = param
        .get("in")
        .and_then(|v| v.as_str())
        .unwrap_or("query")
        .to_string();
    let param_type = param
        .get("schema")
        .and_then(|s| s.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or("string")
        .to_string();
    let required = param
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let description = param
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(ParamInfo {
        name,
        location,
        param_type,
        required,
        description,
    })
}

/// Extract request body JSON schema, resolving `$ref` if present.
fn extract_body_schema(operation: &Value, components: Option<&Value>) -> Option<Value> {
    let schema = operation
        .get("requestBody")?
        .get("content")?
        .get("application/json")?
        .get("schema")?;

    Some(resolve_schema_ref(schema, components).clone())
}

/// Extract response schema from the first 2xx response, resolving `$ref` if present.
fn extract_response_schema(operation: &Value, components: Option<&Value>) -> Option<Value> {
    let responses = operation.get("responses")?.as_object()?;

    // Find first 2xx response
    let response = ["200", "201", "202", "204"]
        .iter()
        .find_map(|code| responses.get(*code))?;

    let schema = response
        .get("content")?
        .get("application/json")?
        .get("schema")?;

    Some(resolve_schema_ref(schema, components).clone())
}

/// Compute the React hook name from an operation ID (e.g., "listItems" → "useListItems").
fn compute_hook_name(operation_id: &str) -> String {
    format!("use{}", capitalize_first(operation_id))
}

pub(crate) fn parse_openapi_operations(openapi: &Value) -> Result<Vec<RouteInfo>, String> {
    let mut routes = Vec::new();

    let paths = openapi
        .get("paths")
        .and_then(|p| p.as_object())
        .ok_or_else(|| "OpenAPI schema missing 'paths' object".to_string())?;

    let components = openapi.get("components");

    for (path, path_item) in paths {
        let methods_obj = path_item
            .as_object()
            .ok_or_else(|| format!("Path '{path}' is not an object"))?;

        for (method, operation) in methods_obj {
            let method_upper = method.to_uppercase();
            if !matches!(
                method_upper.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                continue;
            }

            let operation_obj = operation
                .as_object()
                .ok_or_else(|| format!("Operation '{method}' at path '{path}' is not an object"))?;

            let operation_id = operation_obj
                .get("operationId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let description = operation_obj
                .get("summary")
                .or_else(|| operation_obj.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let hook_name = compute_hook_name(&operation_id);
            let parameters = extract_parameters(operation, path_item);
            let request_body_schema = extract_body_schema(operation, components);
            let response_schema = extract_response_schema(operation, components);

            routes.push(RouteInfo {
                id: operation_id,
                method: method_upper,
                path: path.clone(),
                description,
                hook_name,
                parameters,
                request_body_schema,
                response_schema,
            });
        }
    }

    Ok(routes)
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn generate_query_example(
    operation_id: &str,
    path: &str,
    parameters: &[ParamInfo],
    _response_schema: Option<&Value>,
) -> String {
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");
    let suspense_hook_name = format!("{hook_name}Suspense");

    // Build hook call args based on parameters
    let param_names: Vec<&str> = parameters
        .iter()
        .filter(|p| p.location == "path" || p.location == "query")
        .map(|p| p.name.as_str())
        .collect();

    let hook_call_args = if param_names.is_empty() {
        "selector()".to_string()
    } else {
        let params_obj = param_names.join(", ");
        format!("{{ params: {{ {params_obj} }}, ...selector() }}")
    };

    // Build props for component parameters
    let props_arg = if param_names.is_empty() {
        String::new()
    } else {
        let props: Vec<String> = param_names.iter().map(|n| format!("{n}: string")).collect();
        format!(
            "{{ {} }}: {{ {} }}",
            param_names.join(", "),
            props.join("; ")
        )
    };

    let prop_forwarding = if param_names.is_empty() {
        String::new()
    } else {
        let fwd = param_names
            .iter()
            .map(|n| format!("{n}={{{n}}}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(" {fwd}")
    };

    format!(
        r#"// === Query: {operation_id} ===
// Route: GET {path}
// Hook: {suspense_hook_name} | {hook_name}

// --- Suspense variant (recommended) ---
import {{ Suspense }} from "react";
import {{ QueryErrorResetBoundary }} from "@tanstack/react-query";
import {{ ErrorBoundary }} from "react-error-boundary";
import {{ Skeleton }} from "@/components/ui/skeleton";
import {{ {suspense_hook_name} }} from "@/lib/api";
import selector from "@/lib/selector";

function {capitalized}Content({props_arg}) {{
  const {{ data }} = {suspense_hook_name}({hook_call_args});
  return <div>{{/* render data */}}</div>;
}}

export function {capitalized}Page({props_arg}) {{
  return (
    <QueryErrorResetBoundary>
      {{({{ reset }}) => (
        <ErrorBoundary onReset={{reset}} fallbackRender={{({{ resetErrorBoundary }}) => (
          <div>
            <p>Something went wrong</p>
            <button onClick={{resetErrorBoundary}}>Try again</button>
          </div>
        )}}>
          <Suspense fallback={{<Skeleton className="h-48 w-full" />}}>
            <{capitalized}Content{prop_forwarding} />
          </Suspense>
        </ErrorBoundary>
      )}}
    </QueryErrorResetBoundary>
  );
}}

// --- Standard variant ---
// const {{ data, isLoading, error }} = {hook_name}({hook_call_args});"#
    )
}

fn generate_mutation_example(
    operation_id: &str,
    path: &str,
    method: &str,
    parameters: &[ParamInfo],
    _request_body_schema: Option<&Value>,
) -> String {
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");

    // Derive related query key from path prefix for cache invalidation
    let related_query_key = derive_related_query_key(path);

    // Build mutate call args
    let path_params: Vec<&str> = parameters
        .iter()
        .filter(|p| p.location == "path")
        .map(|p| p.name.as_str())
        .collect();

    let mutate_args = if path_params.is_empty() {
        "{ data: { /* request body */ } }".to_string()
    } else {
        let path_params_obj = path_params.join(", ");
        format!("{{ params: {{ {path_params_obj} }}, data: {{ /* request body */ }} }}")
    };

    format!(
        r#"// === Mutation: {operation_id} ===
// Route: {method} {path}
// Hook: {hook_name}

import {{ {hook_name} }} from "@/lib/api";
import {{ useQueryClient }} from "@tanstack/react-query";

function {capitalized}Button() {{
  const queryClient = useQueryClient();
  const {{ mutate, isPending }} = {hook_name}({{
    mutation: {{
      onSuccess: () => {{
        queryClient.invalidateQueries({{ queryKey: ["{related_query_key}"] }});
      }},
    }},
  }});

  return (
    <button onClick={{() => mutate({mutate_args})}} disabled={{isPending}}>
      {{isPending ? "Submitting..." : "Submit"}}
    </button>
  );
}}"#
    )
}

/// Derive a related query key from a path for cache invalidation.
/// e.g., "/api/jobs/{id}" → "listJobs", "/api/items" → "listItems"
fn derive_related_query_key(path: &str) -> String {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect();
    // Take the last non-parameter segment as the resource name
    let resource = segments.last().copied().unwrap_or("items");
    format!("list{}", capitalize_first(resource))
}

impl ApxServer {
    pub async fn handle_check(&self, args: AppPathArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::OutputMode;
        use apx_core::ops::check::run_check;

        tool_response! {
            struct CheckResponse {
                status: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                errors: Option<String>,
            }
        }

        let response = match run_check(&path, OutputMode::Quiet).await {
            Ok(()) => CheckResponse {
                status: "passed".to_string(),
                errors: None,
            },
            Err(e) => CheckResponse {
                status: "failed".to_string(),
                errors: Some(e),
            },
        };

        if response.errors.is_some() {
            Ok(CallToolResult::from_serializable_error(&response))
        } else {
            Ok(CallToolResult::from_serializable(&response))
        }
    }

    pub async fn handle_refresh_openapi(
        &self,
        args: AppPathArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        match apx_core::api_generator::generate_openapi(&path).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                "OpenAPI regenerated",
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    pub async fn handle_get_route_info(
        &self,
        args: GetRouteInfoArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::read_project_metadata;
        use apx_core::interop::generate_openapi_spec;

        let metadata = match read_project_metadata(&path) {
            Ok(m) => m,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(e)])),
        };

        let openapi_content = match generate_openapi_spec(
            &path,
            &metadata.app_entrypoint,
            &metadata.app_slug,
        )
        .await
        {
            Ok((content, _)) => content,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to generate OpenAPI spec: {e}"
                ))]));
            }
        };

        let openapi: Value = match serde_json::from_str(&openapi_content) {
            Ok(spec) => spec,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to parse OpenAPI schema: {e}"
                ))]));
            }
        };

        let paths = match openapi.get("paths").and_then(|p| p.as_object()) {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "OpenAPI schema missing 'paths' object",
                )]));
            }
        };

        let components = openapi.get("components");

        // Find the operation and capture all context
        let mut found = None;
        for (route_path, path_item) in paths {
            if let Some(methods_obj) = path_item.as_object() {
                for (method, operation) in methods_obj {
                    if let Some(operation_obj) = operation.as_object()
                        && let Some(op_id) =
                            operation_obj.get("operationId").and_then(|v| v.as_str())
                        && op_id == args.operation_id
                    {
                        let method_upper = method.to_uppercase();
                        let parameters = extract_parameters(operation, path_item);
                        let body_schema = extract_body_schema(operation, components);
                        let resp_schema = extract_response_schema(operation, components);
                        found = Some((
                            route_path.clone(),
                            method_upper,
                            parameters,
                            body_schema,
                            resp_schema,
                        ));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }

        let (route_path, method, parameters, body_schema, resp_schema) = match found {
            Some(f) => f,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Operation ID '{}' not found in OpenAPI schema",
                    args.operation_id
                ))]));
            }
        };

        let example = if method == "GET" {
            generate_query_example(
                &args.operation_id,
                &route_path,
                &parameters,
                resp_schema.as_ref(),
            )
        } else {
            generate_mutation_example(
                &args.operation_id,
                &route_path,
                &method,
                &parameters,
                body_schema.as_ref(),
            )
        };

        tool_response! {
            struct RouteInfoResponse {
                operation_id: String,
                method: String,
                path: String,
                parameters: Vec<ParamInfo>,
                #[serde(skip_serializing_if = "Option::is_none")]
                request_body_schema: Option<Value>,
                #[serde(skip_serializing_if = "Option::is_none")]
                response_schema: Option<Value>,
                example: String,
            }
        }

        let response = RouteInfoResponse {
            operation_id: args.operation_id,
            method,
            path: route_path,
            parameters,
            request_body_schema: body_schema,
            response_schema: resp_schema,
            example,
        };

        Ok(CallToolResult::from_serializable(&response))
    }

    pub async fn handle_routes(
        &self,
        args: AppPathArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::read_project_metadata;
        use apx_core::interop::generate_openapi_spec;

        let metadata = match read_project_metadata(&path) {
            Ok(m) => m,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(e)])),
        };

        let (openapi_content, _) = match generate_openapi_spec(
            &path,
            &metadata.app_entrypoint,
            &metadata.app_slug,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to generate OpenAPI spec: {e}"
                ))]));
            }
        };

        let openapi: Value = match serde_json::from_str(&openapi_content) {
            Ok(spec) => spec,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to parse OpenAPI schema: {e}"
                ))]));
            }
        };

        match parse_openapi_operations(&openapi) {
            Ok(routes) => {
                tool_response! {
                    struct RoutesResponse {
                        routes: Vec<RouteInfo>,
                    }
                }
                Ok(CallToolResult::from_serializable(&RoutesResponse {
                    routes,
                }))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_openapi_operations_basic() {
        let openapi = serde_json::json!({
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List all items"
                    },
                    "post": {
                        "operationId": "createItem",
                        "description": "Create a new item"
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 2);

        let get_route = routes.iter().find(|r| r.method == "GET").unwrap();
        assert_eq!(get_route.id, "listItems");
        assert_eq!(get_route.description, "List all items");
        assert_eq!(get_route.hook_name, "useListItems");
        assert!(get_route.parameters.is_empty());

        let post_route = routes.iter().find(|r| r.method == "POST").unwrap();
        assert_eq!(post_route.id, "createItem");
        assert_eq!(post_route.description, "Create a new item");
        assert_eq!(post_route.hook_name, "useCreateItem");
    }

    #[test]
    fn parse_openapi_operations_skips_non_methods() {
        let openapi = serde_json::json!({
            "paths": {
                "/items": {
                    "parameters": [{"name": "id"}],
                    "get": {
                        "operationId": "listItems",
                        "summary": "List items"
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
    }

    #[test]
    fn parse_openapi_operations_missing_paths() {
        let openapi = serde_json::json!({});
        assert!(parse_openapi_operations(&openapi).is_err());
    }

    #[test]
    fn parse_openapi_extracts_parameters() {
        let openapi = serde_json::json!({
            "paths": {
                "/items/{id}": {
                    "get": {
                        "operationId": "getItem",
                        "summary": "Get item by ID",
                        "parameters": [
                            {
                                "name": "id",
                                "in": "path",
                                "required": true,
                                "schema": { "type": "integer" }
                            },
                            {
                                "name": "page",
                                "in": "query",
                                "required": false,
                                "schema": { "type": "integer" },
                                "description": "Page number"
                            }
                        ]
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].parameters.len(), 2);

        let id_param = routes[0]
            .parameters
            .iter()
            .find(|p| p.name == "id")
            .unwrap();
        assert_eq!(id_param.location, "path");
        assert_eq!(id_param.param_type, "integer");
        assert!(id_param.required);

        let page_param = routes[0]
            .parameters
            .iter()
            .find(|p| p.name == "page")
            .unwrap();
        assert_eq!(page_param.location, "query");
        assert!(!page_param.required);
        assert_eq!(page_param.description.as_deref(), Some("Page number"));
    }

    #[test]
    fn parse_openapi_extracts_request_body() {
        let openapi = serde_json::json!({
            "paths": {
                "/items": {
                    "post": {
                        "operationId": "createItem",
                        "summary": "Create item",
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "name": { "type": "string" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 1);
        assert!(routes[0].request_body_schema.is_some());
        let schema = routes[0].request_body_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["name"].is_object());
    }

    #[test]
    fn parse_openapi_extracts_response_schema() {
        let openapi = serde_json::json!({
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List items",
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "type": "array",
                                            "items": { "type": "object" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 1);
        assert!(routes[0].response_schema.is_some());
        let schema = routes[0].response_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "array");
    }

    #[test]
    fn parse_openapi_resolves_refs() {
        let openapi = serde_json::json!({
            "components": {
                "schemas": {
                    "Item": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "integer" },
                            "name": { "type": "string" }
                        }
                    }
                }
            },
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List items",
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "$ref": "#/components/schemas/Item"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes.len(), 1);
        let schema = routes[0].response_schema.as_ref().unwrap();
        // $ref should be resolved to the inline schema
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["id"].is_object());
    }

    #[test]
    fn parse_openapi_computes_hook_name() {
        let openapi = serde_json::json!({
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List items"
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes[0].hook_name, "useListItems");
    }

    #[test]
    fn parse_openapi_merges_path_and_op_params() {
        let openapi = serde_json::json!({
            "paths": {
                "/items/{id}": {
                    "parameters": [
                        {
                            "name": "id",
                            "in": "path",
                            "required": true,
                            "schema": { "type": "string" },
                            "description": "Path-level ID"
                        }
                    ],
                    "get": {
                        "operationId": "getItem",
                        "summary": "Get item",
                        "parameters": [
                            {
                                "name": "id",
                                "in": "path",
                                "required": true,
                                "schema": { "type": "integer" },
                                "description": "Op-level ID"
                            }
                        ]
                    }
                }
            }
        });

        let routes = parse_openapi_operations(&openapi).unwrap();
        assert_eq!(routes[0].parameters.len(), 1);
        // Operation-level param should override path-level
        assert_eq!(routes[0].parameters[0].param_type, "integer");
        assert_eq!(
            routes[0].parameters[0].description.as_deref(),
            Some("Op-level ID")
        );
    }

    #[test]
    fn routes_response_structured_content_is_object() {
        // MCP spec requires structuredContent to be a JSON object, not an array.
        // Vec<RouteInfo> doesn't implement StructuredObject, so it can't be
        // passed to from_serializable — this is enforced at compile time.
        // The RoutesResponse wrapper ensures the output is always an object.
        tool_response! {
            struct RoutesResponse {
                routes: Vec<RouteInfo>,
            }
        }

        let routes = parse_openapi_operations(&serde_json::json!({
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List all items"
                    }
                }
            }
        }))
        .unwrap();

        let result = CallToolResult::from_serializable(&RoutesResponse { routes });
        let sc = result
            .structured_content
            .expect("structured_content should be set");
        assert!(
            sc.is_object(),
            "structuredContent must be a JSON object, got: {sc}"
        );
        // Verify the routes are nested under "routes" key
        assert!(sc.get("routes").unwrap().is_array());
    }

    #[test]
    fn generate_query_example_no_params() {
        let example = generate_query_example("listItems", "/api/items", &[], None);
        assert!(example.contains("Suspense"));
        assert!(example.contains("ErrorBoundary"));
        assert!(example.contains("Skeleton"));
        assert!(example.contains("selector()"));
        assert!(example.contains("useListItemsSuspense"));
    }

    #[test]
    fn generate_query_example_with_params() {
        let params = vec![ParamInfo {
            name: "page".to_string(),
            location: "query".to_string(),
            param_type: "integer".to_string(),
            required: false,
            description: None,
        }];
        let example = generate_query_example("listItems", "/api/items", &params, None);
        assert!(example.contains("{ params: { page }, ...selector() }"));
    }

    #[test]
    fn generate_mutation_example_basic() {
        let example = generate_mutation_example("createItem", "/api/items", "POST", &[], None);
        assert!(example.contains("useCreateItem"));
        assert!(example.contains("onSuccess"));
        assert!(example.contains("invalidateQueries"));
        assert!(example.contains("useQueryClient"));
    }

    #[test]
    fn generate_mutation_example_with_path_params() {
        let params = vec![ParamInfo {
            name: "id".to_string(),
            location: "path".to_string(),
            param_type: "integer".to_string(),
            required: true,
            description: None,
        }];
        let example =
            generate_mutation_example("updateItem", "/api/items/{id}", "PUT", &params, None);
        assert!(example.contains("params: { id }"));
    }
}
