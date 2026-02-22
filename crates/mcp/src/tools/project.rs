use crate::server::ApxServer;
use crate::tools::openapi::{
    ParamInfo, RouteInfo, extract_body_schema, extract_parameters, extract_response_schema,
    generate_mutation_example, generate_query_example, parse_openapi_operations,
};
use crate::tools::{AppPathArgs, ToolError, ToolResultExt};
use crate::validation::ValidatedAppPath;
use rmcp::model::*;
use rmcp::schemars;
use serde_json::Value;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRouteInfoArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Operation ID from the OpenAPI schema (e.g., "listItems", "createItem")
    pub operation_id: String,
}

impl ApxServer {
    pub async fn handle_check(&self, args: AppPathArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = ValidatedAppPath::try_from_str(&args.app_path)?;

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
        let path = ValidatedAppPath::try_from_str(&args.app_path)?;

        match apx_core::api_generator::generate_openapi(&path).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                "OpenAPI regenerated",
            )])),
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }

    pub async fn handle_get_route_info(
        &self,
        args: GetRouteInfoArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = ValidatedAppPath::try_from_str(&args.app_path)?;

        use apx_core::common::read_project_metadata;
        use apx_core::interop::generate_openapi_spec;

        let metadata = match read_project_metadata(&path) {
            Ok(m) => m,
            Err(e) => return ToolError::OperationFailed(e).into_result(),
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
                return ToolError::OperationFailed(format!("Failed to generate OpenAPI spec: {e}"))
                    .into_result();
            }
        };

        let openapi: Value = match serde_json::from_str(&openapi_content) {
            Ok(spec) => spec,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to parse OpenAPI schema: {e}"))
                    .into_result();
            }
        };

        let paths = match openapi.get("paths").and_then(|p| p.as_object()) {
            Some(p) => p,
            None => {
                return ToolError::OperationFailed(
                    "OpenAPI schema missing 'paths' object".to_string(),
                )
                .into_result();
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
                return ToolError::OperationFailed(format!(
                    "Operation ID '{}' not found in OpenAPI schema",
                    args.operation_id
                ))
                .into_result();
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
        let path = ValidatedAppPath::try_from_str(&args.app_path)?;

        use apx_core::common::read_project_metadata;
        use apx_core::interop::generate_openapi_spec;

        let metadata = match read_project_metadata(&path) {
            Ok(m) => m,
            Err(e) => return ToolError::OperationFailed(e).into_result(),
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
                return ToolError::OperationFailed(format!("Failed to generate OpenAPI spec: {e}"))
                    .into_result();
            }
        };

        let openapi: Value = match serde_json::from_str(&openapi_content) {
            Ok(spec) => spec,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to parse OpenAPI schema: {e}"))
                    .into_result();
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
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }
}
