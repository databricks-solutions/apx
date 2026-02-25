use apx_core::openapi::capitalize_first;
use apx_core::openapi::spec::{Components, OpenApiSpec, Operation, Parameter, Schema};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, Clone)]
pub(crate) struct ParamInfo {
    pub(crate) name: String,
    pub(crate) location: String,
    pub(crate) param_type: String,
    pub(crate) required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize)]
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

/// Convert a core `Parameter` to MCP's `ParamInfo`.
fn param_from_spec(p: &Parameter) -> ParamInfo {
    let param_type = p
        .schema
        .as_ref()
        .and_then(|s| match &s.schema_type {
            Some(apx_core::openapi::spec::SchemaType::Single(t)) => Some(t.as_str()),
            _ => None,
        })
        .unwrap_or("string")
        .to_string();

    ParamInfo {
        name: p.name.clone(),
        location: p.location.clone(),
        param_type,
        required: p.required,
        description: p.description.clone(),
    }
}

/// Merge path-level and operation-level parameters.
/// Operation-level params override path-level on name+location match.
pub(crate) fn merge_parameters(
    path_params: Option<&Vec<Parameter>>,
    op_params: Option<&Vec<Parameter>>,
) -> Vec<ParamInfo> {
    let mut params: Vec<ParamInfo> = Vec::new();

    if let Some(pp) = path_params {
        for p in pp {
            params.push(param_from_spec(p));
        }
    }

    if let Some(op) = op_params {
        for p in op {
            let info = param_from_spec(p);
            params.retain(|existing| {
                !(existing.name == info.name && existing.location == info.location)
            });
            params.push(info);
        }
    }

    params
}

/// Resolve a `$ref` in a Schema against components, returning raw JSON Value.
fn resolve_schema(schema: &Schema, components: Option<&Components>) -> Value {
    if let Some(ref_path) = &schema.ref_path
        && let Some(name) = ref_path.strip_prefix("#/components/schemas/")
        && let Some(resolved) = components
            .and_then(|c| c.schemas.as_ref())
            .and_then(|s| s.get(name))
    {
        return serde_json::to_value(resolved).unwrap_or_default();
    }
    serde_json::to_value(schema).unwrap_or_default()
}

/// Extract request body JSON schema from a typed Operation.
pub(crate) fn body_schema_from_spec(
    op: &Operation,
    components: Option<&Components>,
) -> Option<Value> {
    let schema = op
        .request_body
        .as_ref()?
        .content
        .as_ref()?
        .get("application/json")?
        .schema
        .as_ref()?;
    Some(resolve_schema(schema, components))
}

/// Extract response schema from the first 2xx response of a typed Operation.
pub(crate) fn response_schema_from_spec(
    op: &Operation,
    components: Option<&Components>,
) -> Option<Value> {
    let response = ["200", "201", "202", "204"]
        .iter()
        .find_map(|code| op.responses.get(*code))?;
    let schema = response
        .content
        .as_ref()?
        .get("application/json")?
        .schema
        .as_ref()?;
    Some(resolve_schema(schema, components))
}

/// Compute the React hook name from an operation ID (e.g., "listItems" -> "useListItems").
fn compute_hook_name(operation_id: &str) -> String {
    format!("use{}", capitalize_first(operation_id))
}

pub(crate) fn parse_openapi_operations(spec: &OpenApiSpec) -> Result<Vec<RouteInfo>, String> {
    let mut routes = Vec::new();
    let components = spec.components.as_ref();

    for (path, path_item) in &spec.paths {
        let methods: Vec<(&str, Option<&Operation>)> = vec![
            ("GET", path_item.get.as_ref()),
            ("POST", path_item.post.as_ref()),
            ("PUT", path_item.put.as_ref()),
            ("PATCH", path_item.patch.as_ref()),
            ("DELETE", path_item.delete.as_ref()),
            ("HEAD", path_item.head.as_ref()),
            ("OPTIONS", path_item.options.as_ref()),
        ];

        for (method, op) in methods {
            let Some(operation) = op else { continue };

            let operation_id = operation
                .operation_id
                .as_deref()
                .unwrap_or("unknown")
                .to_string();

            let description = operation
                .summary
                .as_deref()
                .or(operation.description.as_deref())
                .unwrap_or("")
                .to_string();

            let hook_name = compute_hook_name(&operation_id);
            let parameters =
                merge_parameters(path_item.parameters.as_ref(), operation.parameters.as_ref());
            let request_body_schema = body_schema_from_spec(operation, components);
            let response_schema = response_schema_from_spec(operation, components);

            routes.push(RouteInfo {
                id: operation_id,
                method: method.to_string(),
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

pub(crate) fn generate_query_example(
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

pub(crate) fn generate_mutation_example(
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
/// e.g., "/api/jobs/{id}" -> "listJobs", "/api/items" -> "listItems"
fn derive_related_query_key(path: &str) -> String {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect();
    // Take the last non-parameter segment as the resource name
    let resource = segments.last().copied().unwrap_or("items");
    format!("list{}", capitalize_first(resource))
}

#[cfg(test)]
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tools::ToolResultExt;
    use rmcp::model::CallToolResult;

    fn parse_test_spec(json: Value) -> OpenApiSpec {
        serde_json::from_value(json).unwrap()
    }

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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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
                    "parameters": [{"name": "id", "in": "query"}],
                    "get": {
                        "operationId": "listItems",
                        "summary": "List items"
                    }
                }
            }
        });

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
    }

    #[test]
    fn parse_openapi_operations_missing_paths() {
        let json = "{}";
        let result = OpenApiSpec::from_json(json);
        assert!(result.is_err(), "Expected error for missing paths");
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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

        let spec = parse_test_spec(openapi);
        let routes = parse_openapi_operations(&spec).unwrap();
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
        // passed to from_serializable -- this is enforced at compile time.
        // The RoutesResponse wrapper ensures the output is always an object.
        tool_response! {
            struct RoutesResponse {
                routes: Vec<RouteInfo>,
            }
        }

        let spec = parse_test_spec(serde_json::json!({
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List all items"
                    }
                }
            }
        }));
        let routes = parse_openapi_operations(&spec).unwrap();

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
