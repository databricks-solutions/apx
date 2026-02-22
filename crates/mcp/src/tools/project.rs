use crate::server::ApxServer;
use crate::tools::openapi::{
    ParamInfo, RouteInfo, body_schema_from_spec, generate_mutation_example, generate_query_example,
    merge_parameters, parse_openapi_operations, response_schema_from_spec,
};
use crate::tools::{AppPathArgs, ToolError, ToolResultExt};
use crate::validation::validated_app_path;
use apx_core::openapi::spec::OpenApiSpec;
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
        let path = validated_app_path(&args.app_path)?;

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
        let path = validated_app_path(&args.app_path)?;

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
        let path = validated_app_path(&args.app_path)?;

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

        let spec = match OpenApiSpec::from_json(&openapi_content) {
            Ok(s) => s,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to parse OpenAPI schema: {e}"))
                    .into_result();
            }
        };

        let components = spec.components.as_ref();

        // Find the operation and capture all context
        let mut found = None;
        for (route_path, path_item) in &spec.paths {
            let methods: Vec<(&str, Option<&apx_core::openapi::spec::Operation>)> = vec![
                ("GET", path_item.get.as_ref()),
                ("POST", path_item.post.as_ref()),
                ("PUT", path_item.put.as_ref()),
                ("PATCH", path_item.patch.as_ref()),
                ("DELETE", path_item.delete.as_ref()),
                ("HEAD", path_item.head.as_ref()),
                ("OPTIONS", path_item.options.as_ref()),
            ];

            for (method, op) in methods {
                if let Some(operation) = op
                    && operation.operation_id.as_deref() == Some(&args.operation_id)
                {
                    let parameters = merge_parameters(
                        path_item.parameters.as_ref(),
                        operation.parameters.as_ref(),
                    );
                    let body_schema = body_schema_from_spec(operation, components);
                    let resp_schema = response_schema_from_spec(operation, components);
                    found = Some((
                        route_path.clone(),
                        method.to_string(),
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
        let path = validated_app_path(&args.app_path)?;

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

        let spec = match OpenApiSpec::from_json(&openapi_content) {
            Ok(s) => s,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to parse OpenAPI schema: {e}"))
                    .into_result();
            }
        };

        match parse_openapi_operations(&spec) {
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
