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

#[derive(Serialize)]
struct RouteInfo {
    id: String,
    method: String,
    path: String,
    description: String,
}

fn parse_openapi_operations(openapi: &Value) -> Result<Vec<RouteInfo>, String> {
    let mut routes = Vec::new();

    let paths = openapi
        .get("paths")
        .and_then(|p| p.as_object())
        .ok_or_else(|| "OpenAPI schema missing 'paths' object".to_string())?;

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

            routes.push(RouteInfo {
                id: operation_id,
                method: method_upper,
                path: path.clone(),
                description,
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

fn generate_query_example(operation_id: &str) -> String {
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");
    let suspense_hook_name = format!("{hook_name}Suspense");
    let result_type = format!("{capitalized}QueryResult");
    let error_type = format!("{capitalized}QueryError");

    format!(
        r#"// Standard query hook
import {{ {hook_name} }} from "@/lib/api";
import selector from "@/lib/selector";

const Component = () => {{
  const {{ data, isLoading, error }} = {hook_name}(selector());

  if (isLoading) return <div>Loading...</div>;
  if (error) return <div>Error: {{error.message}}</div>;

  return <div>{{/* render data */}}</div>;
}};

// Suspense query hook (use with React Suspense boundary)
import {{ {suspense_hook_name} }} from "@/lib/api";
import selector from "@/lib/selector";

const SuspenseComponent = () => {{
  // No loading/error states needed - handled by Suspense boundary
  const {{ data }} = {suspense_hook_name}(selector());
  return <div>{{/* render data */}}</div>;
}};

// Usage with Suspense boundary:
// <Suspense fallback={{<Loading />}}>
//   <SuspenseComponent />
// </Suspense>

// Available types for this query:
// import type {{ {result_type}, {error_type} }} from "@/lib/api";"#
    )
}

fn generate_mutation_example(operation_id: &str) -> String {
    let capitalized = capitalize_first(operation_id);
    let hook_name = format!("use{capitalized}");
    let body_type = format!("{capitalized}MutationBody");
    let result_type = format!("{capitalized}MutationResult");
    let error_type = format!("{capitalized}MutationError");

    format!(
        r#"import {{ {hook_name} }} from "@/lib/api";

const Component = () => {{
  const {{ mutate, isPending }} = {hook_name}();

  const handleSubmit = () => {{
    mutate({{ data: {{ /* request body */ }} }});
  }};

  return <button onClick={{handleSubmit}}>Submit</button>;
}};

// Available types for this mutation:
// import type {{ {body_type}, {result_type}, {error_type} }} from "@/lib/api";"#
    )
}

impl ApxServer {
    pub async fn handle_check(&self, args: AppPathArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::OutputMode;
        use apx_core::ops::check::run_check;

        #[derive(Serialize)]
        struct CheckResponse {
            status: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            errors: Option<String>,
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
            match serde_json::to_value(&response) {
                Ok(value) => Ok(CallToolResult::structured_error(value)),
                Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to serialize response: {e}"
                ))])),
            }
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

        let mut found_method = None;
        for (_, path_item) in paths {
            if let Some(methods_obj) = path_item.as_object() {
                for (method, operation) in methods_obj {
                    if let Some(operation_obj) = operation.as_object()
                        && let Some(op_id) =
                            operation_obj.get("operationId").and_then(|v| v.as_str())
                        && op_id == args.operation_id
                    {
                        found_method = Some(method.to_uppercase());
                        break;
                    }
                }
                if found_method.is_some() {
                    break;
                }
            }
        }

        let method = match found_method {
            Some(m) => m,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Operation ID '{}' not found in OpenAPI schema",
                    args.operation_id
                ))]));
            }
        };

        let example = if method == "GET" {
            generate_query_example(&args.operation_id)
        } else {
            generate_mutation_example(&args.operation_id)
        };

        #[derive(Serialize)]
        struct RouteInfoResponse {
            operation_id: String,
            method: String,
            example: String,
        }

        let response = RouteInfoResponse {
            operation_id: args.operation_id,
            method,
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
            Ok(routes) => Ok(CallToolResult::from_serializable(&routes)),
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

        let post_route = routes.iter().find(|r| r.method == "POST").unwrap();
        assert_eq!(post_route.id, "createItem");
        assert_eq!(post_route.description, "Create a new item");
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
}
