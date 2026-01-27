//! TypeScript code emitter for OpenAPI specifications.
//!
//! This module generates TypeScript code from parsed OpenAPI specs including:
//! - Type definitions (interfaces, type aliases, enums)
//! - Fetch-based API client functions
//! - React Query hooks (useQuery, useSuspenseQuery, useMutation)

use std::collections::HashMap;

use crate::openapi::spec::{
    AdditionalProperties, OpenApiSpec, Operation, Parameter, PathItem, Schema, SchemaType,
};

/// Generate TypeScript code from an OpenAPI JSON string.
pub fn generate(openapi_json: &str) -> Result<String, String> {
    let spec = OpenApiSpec::from_json(openapi_json)?;
    let mut output = String::new();

    // 1. Emit imports
    output.push_str(&emit_imports());

    // 2. Emit types from component schemas
    if let Some(components) = &spec.components {
        if let Some(schemas) = &components.schemas {
            output.push_str(&emit_types(schemas)?);
        }
    }

    // 3. Emit operations (fetch functions + hooks)
    output.push_str(&emit_operations(&spec)?);

    Ok(output)
}

/// Emit the import statements for React Query.
fn emit_imports() -> String {
    r#"import {
  useQuery,
  useSuspenseQuery,
  useMutation,
} from "@tanstack/react-query";
import type {
  UseQueryOptions,
  UseSuspenseQueryOptions,
  UseMutationOptions,
} from "@tanstack/react-query";

"#
    .to_string()
}

/// Emit TypeScript types from OpenAPI component schemas.
fn emit_types(schemas: &HashMap<String, Schema>) -> Result<String, String> {
    let mut output = String::new();

    // Sort schemas by name for deterministic output
    let mut schema_names: Vec<_> = schemas.keys().collect();
    schema_names.sort();

    for name in schema_names {
        let schema = schemas
            .get(name)
            .ok_or_else(|| format!("Schema {name} not found"))?;
        output.push_str(&emit_type_definition(name, schema)?);
        output.push('\n');
    }

    Ok(output)
}

/// Emit a single type definition.
fn emit_type_definition(name: &str, schema: &Schema) -> Result<String, String> {
    // Check if it's an enum
    if let Some(enum_values) = &schema.enum_values {
        return Ok(emit_string_enum(name, enum_values));
    }

    // Check if it's an object with properties
    if let Some(properties) = &schema.properties {
        return Ok(emit_interface(name, properties, schema.required.as_ref()));
    }

    // For other types, emit a type alias
    let ts_type = schema_to_ts_type(schema)?;
    Ok(format!("export type {name} = {ts_type};\n"))
}

/// Emit a string enum as a const object + type.
fn emit_string_enum(name: &str, values: &[String]) -> String {
    let mut output = String::new();

    // Emit const object
    output.push_str(&format!("export const {name} = {{\n"));
    for value in values {
        output.push_str(&format!("  {value}: \"{value}\",\n"));
    }
    output.push_str("} as const;\n\n");

    // Emit type
    output.push_str(&format!(
        "export type {name} = (typeof {name})[keyof typeof {name}];\n"
    ));

    output
}

/// Emit an interface definition.
fn emit_interface(
    name: &str,
    properties: &HashMap<String, Schema>,
    required: Option<&Vec<String>>,
) -> String {
    let required_set: std::collections::HashSet<_> = required
        .map(|r| r.iter().collect())
        .unwrap_or_default();

    let mut output = format!("export interface {name} {{\n");

    // Sort properties by name for deterministic output
    let mut prop_names: Vec<_> = properties.keys().collect();
    prop_names.sort();

    for prop_name in prop_names {
        let prop_schema = properties.get(prop_name).unwrap_or(&Schema {
            schema_type: None,
            ref_path: None,
            properties: None,
            required: None,
            items: None,
            enum_values: None,
            any_of: None,
            one_of: None,
            additional_properties: None,
            format: None,
        });

        let ts_type = schema_to_ts_type(prop_schema).unwrap_or_else(|_| "unknown".to_string());
        let optional = if required_set.contains(prop_name) {
            ""
        } else {
            "?"
        };

        output.push_str(&format!("  {prop_name}{optional}: {ts_type};\n"));
    }

    output.push_str("}\n");
    output
}

/// Convert a schema to a TypeScript type string.
fn schema_to_ts_type(schema: &Schema) -> Result<String, String> {
    // Handle $ref
    if let Some(ref_path) = &schema.ref_path {
        return Ok(ref_to_type_name(ref_path));
    }

    // Handle anyOf (union types, often used for nullable)
    if let Some(any_of) = &schema.any_of {
        return Ok(emit_union_type(any_of)?);
    }

    // Handle oneOf (discriminated unions)
    if let Some(one_of) = &schema.one_of {
        return Ok(emit_union_type(one_of)?);
    }

    // Handle type
    match &schema.schema_type {
        Some(SchemaType::Single(t)) => schema_type_to_ts(t, schema),
        Some(SchemaType::Multiple(types)) => {
            // Handle nullable types like ["string", "null"]
            let non_null: Vec<_> = types.iter().filter(|t| *t != "null").collect();
            if non_null.len() == 1 {
                let base_type = schema_type_to_ts(non_null[0], schema)?;
                if types.contains(&"null".to_string()) {
                    Ok(format!("{base_type} | null"))
                } else {
                    Ok(base_type)
                }
            } else {
                // Multiple non-null types
                let ts_types: Result<Vec<_>, _> = non_null
                    .iter()
                    .map(|t| schema_type_to_ts(t, schema))
                    .collect();
                let ts_types = ts_types?;
                if types.contains(&"null".to_string()) {
                    Ok(format!("{} | null", ts_types.join(" | ")))
                } else {
                    Ok(ts_types.join(" | "))
                }
            }
        }
        None => {
            // No type specified, check for additionalProperties (Record type)
            if let Some(additional) = &schema.additional_properties {
                match additional {
                    AdditionalProperties::Bool(true) => Ok("Record<string, unknown>".to_string()),
                    AdditionalProperties::Bool(false) => Ok("object".to_string()),
                    AdditionalProperties::Schema(s) => {
                        let value_type = schema_to_ts_type(s)?;
                        Ok(format!("Record<string, {value_type}>"))
                    }
                }
            } else {
                Ok("unknown".to_string())
            }
        }
    }
}

/// Convert a single schema type to TypeScript.
fn schema_type_to_ts(schema_type: &str, schema: &Schema) -> Result<String, String> {
    match schema_type {
        "string" => {
            // Check for enum
            if let Some(enum_values) = &schema.enum_values {
                let literals: Vec<_> = enum_values.iter().map(|v| format!("\"{v}\"")).collect();
                Ok(literals.join(" | "))
            } else {
                Ok("string".to_string())
            }
        }
        "number" | "integer" => Ok("number".to_string()),
        "boolean" => Ok("boolean".to_string()),
        "null" => Ok("null".to_string()),
        "array" => {
            if let Some(items) = &schema.items {
                let item_type = schema_to_ts_type(items)?;
                Ok(format!("{item_type}[]"))
            } else {
                Ok("unknown[]".to_string())
            }
        }
        "object" => {
            // Check for additionalProperties
            if let Some(additional) = &schema.additional_properties {
                match additional {
                    AdditionalProperties::Bool(true) => Ok("Record<string, unknown>".to_string()),
                    AdditionalProperties::Bool(false) => Ok("object".to_string()),
                    AdditionalProperties::Schema(s) => {
                        let value_type = schema_to_ts_type(s)?;
                        Ok(format!("Record<string, {value_type}>"))
                    }
                }
            } else if let Some(properties) = &schema.properties {
                // Inline object type
                let mut parts = Vec::new();
                let required_set: std::collections::HashSet<_> = schema
                    .required
                    .as_ref()
                    .map(|r| r.iter().collect())
                    .unwrap_or_default();

                let mut prop_names: Vec<_> = properties.keys().collect();
                prop_names.sort();

                for prop_name in prop_names {
                    let prop_schema = properties.get(prop_name).unwrap_or(&Schema {
                        schema_type: None,
                        ref_path: None,
                        properties: None,
                        required: None,
                        items: None,
                        enum_values: None,
                        any_of: None,
                        one_of: None,
                        additional_properties: None,
                        format: None,
                    });
                    let ts_type =
                        schema_to_ts_type(prop_schema).unwrap_or_else(|_| "unknown".to_string());
                    let optional = if required_set.contains(prop_name) {
                        ""
                    } else {
                        "?"
                    };
                    parts.push(format!("{prop_name}{optional}: {ts_type}"));
                }

                Ok(format!("{{ {} }}", parts.join("; ")))
            } else {
                Ok("Record<string, unknown>".to_string())
            }
        }
        _ => Ok("unknown".to_string()),
    }
}

/// Emit a union type from anyOf/oneOf schemas.
fn emit_union_type(schemas: &[Schema]) -> Result<String, String> {
    let types: Result<Vec<_>, _> = schemas.iter().map(schema_to_ts_type).collect();
    let types = types?;
    Ok(types.join(" | "))
}

/// Extract a type name from a $ref path.
fn ref_to_type_name(ref_path: &str) -> String {
    ref_path
        .strip_prefix("#/components/schemas/")
        .unwrap_or(ref_path)
        .to_string()
}

/// Emit all operations from the spec.
fn emit_operations(spec: &OpenApiSpec) -> Result<String, String> {
    let mut output = String::new();

    // Sort paths for deterministic output
    let mut paths: Vec<_> = spec.paths.iter().collect();
    paths.sort_by_key(|(path, _)| *path);

    for (path, item) in paths {
        output.push_str(&emit_path_operations(path, item)?);
    }

    Ok(output)
}

/// Emit operations for a single path.
fn emit_path_operations(path: &str, item: &PathItem) -> Result<String, String> {
    let mut output = String::new();
    let path_params = item.parameters.as_ref();

    if let Some(op) = &item.get {
        output.push_str(&emit_query_operation(path, "GET", op, path_params)?);
    }
    if let Some(op) = &item.post {
        output.push_str(&emit_mutation_operation(path, "POST", op, path_params)?);
    }
    if let Some(op) = &item.put {
        output.push_str(&emit_mutation_operation(path, "PUT", op, path_params)?);
    }
    if let Some(op) = &item.patch {
        output.push_str(&emit_mutation_operation(path, "PATCH", op, path_params)?);
    }
    if let Some(op) = &item.delete {
        output.push_str(&emit_mutation_operation(path, "DELETE", op, path_params)?);
    }

    Ok(output)
}

/// Get the operation ID or generate one from path and method.
fn get_operation_id(path: &str, method: &str, op: &Operation) -> String {
    if let Some(id) = &op.operation_id {
        return id.clone();
    }

    // Generate from path and method
    let path_parts: Vec<_> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect();

    let base = path_parts.join("_");
    format!("{}_{}", method.to_lowercase(), base)
}

/// Collect all parameters (path-level + operation-level).
fn collect_parameters<'a>(
    op: &'a Operation,
    path_params: Option<&'a Vec<Parameter>>,
) -> Vec<&'a Parameter> {
    let mut params = Vec::new();

    if let Some(pp) = path_params {
        params.extend(pp.iter());
    }

    if let Some(op_params) = &op.parameters {
        params.extend(op_params.iter());
    }

    params
}

/// Get the success response schema (200, 201, etc.).
fn get_success_response_schema(op: &Operation) -> Option<&Schema> {
    for status in ["200", "201", "202"] {
        if let Some(response) = op.responses.get(status) {
            if let Some(content) = &response.content {
                if let Some(media_type) = content.get("application/json") {
                    return media_type.schema.as_ref();
                }
            }
        }
    }
    None
}

/// Get the request body schema.
fn get_request_body_schema(op: &Operation) -> Option<&Schema> {
    if let Some(body) = &op.request_body {
        if let Some(content) = &body.content {
            if let Some(media_type) = content.get("application/json") {
                return media_type.schema.as_ref();
            }
        }
    }
    None
}

/// Check if the response is void (204 No Content).
fn is_void_response(op: &Operation) -> bool {
    op.responses.contains_key("204") && !op.responses.contains_key("200")
}

/// Emit a query operation (GET) with hooks.
fn emit_query_operation(
    path: &str,
    method: &str,
    op: &Operation,
    path_params: Option<&Vec<Parameter>>,
) -> Result<String, String> {
    let op_id = get_operation_id(path, method, op);
    let params = collect_parameters(op, path_params);
    let response_schema = get_success_response_schema(op);
    let response_type = response_schema
        .map(|s| schema_to_ts_type(s))
        .transpose()?
        .unwrap_or_else(|| "unknown".to_string());

    let mut output = String::new();

    // Emit params type if needed
    let has_query_params = params.iter().any(|p| p.location == "query");
    let has_path_params = params.iter().any(|p| p.location == "path");
    let params_type_name = format!("{}Params", capitalize_first(&op_id));

    if has_query_params || has_path_params {
        output.push_str(&emit_params_type(&params_type_name, &params)?);
    }

    // Emit fetch function
    output.push_str(&emit_fetch_function(
        &op_id,
        path,
        method,
        &params,
        None,
        &response_type,
        has_query_params || has_path_params,
        &params_type_name,
    )?);

    // Emit query key function
    output.push_str(&emit_query_key_function(
        &op_id,
        path,
        has_query_params || has_path_params,
        &params_type_name,
    ));

    // Emit useQuery hook
    output.push_str(&emit_use_query_hook(
        &op_id,
        &response_type,
        has_query_params || has_path_params,
        &params_type_name,
    ));

    // Emit useSuspenseQuery hook
    output.push_str(&emit_use_suspense_query_hook(
        &op_id,
        &response_type,
        has_query_params || has_path_params,
        &params_type_name,
    ));

    Ok(output)
}

/// Emit a mutation operation (POST, PUT, PATCH, DELETE).
fn emit_mutation_operation(
    path: &str,
    method: &str,
    op: &Operation,
    path_params: Option<&Vec<Parameter>>,
) -> Result<String, String> {
    let op_id = get_operation_id(path, method, op);
    let params = collect_parameters(op, path_params);
    let response_schema = get_success_response_schema(op);
    let is_void = is_void_response(op);
    let response_type = if is_void {
        "void".to_string()
    } else {
        response_schema
            .map(|s| schema_to_ts_type(s))
            .transpose()?
            .unwrap_or_else(|| "unknown".to_string())
    };

    let body_schema = get_request_body_schema(op);
    let body_type = body_schema
        .map(|s| schema_to_ts_type(s))
        .transpose()?;

    let mut output = String::new();

    // Emit params type if there are path params
    let has_path_params = params.iter().any(|p| p.location == "path");
    let params_type_name = format!("{}Params", capitalize_first(&op_id));

    if has_path_params {
        output.push_str(&emit_params_type(&params_type_name, &params)?);
    }

    // Emit fetch function
    output.push_str(&emit_fetch_function(
        &op_id,
        path,
        method,
        &params,
        body_type.as_deref(),
        &response_type,
        has_path_params,
        &params_type_name,
    )?);

    // Emit useMutation hook
    output.push_str(&emit_use_mutation_hook(
        &op_id,
        &response_type,
        body_type.as_deref(),
        has_path_params,
        &params_type_name,
    ));

    Ok(output)
}

/// Emit a params type interface.
fn emit_params_type(name: &str, params: &[&Parameter]) -> Result<String, String> {
    let mut output = format!("export interface {name} {{\n");

    for param in params {
        if param.location == "header" {
            continue; // Skip header params
        }

        let ts_type = param
            .schema
            .as_ref()
            .map(schema_to_ts_type)
            .transpose()?
            .unwrap_or_else(|| "string".to_string());

        let optional = if param.required { "" } else { "?" };
        output.push_str(&format!("  {}{optional}: {ts_type};\n", param.name));
    }

    output.push_str("}\n\n");
    Ok(output)
}

/// Emit a fetch function.
#[allow(clippy::too_many_arguments)]
fn emit_fetch_function(
    op_id: &str,
    path: &str,
    method: &str,
    params: &[&Parameter],
    body_type: Option<&str>,
    response_type: &str,
    has_params: bool,
    params_type_name: &str,
) -> Result<String, String> {
    let mut output = String::new();

    // Function signature
    let mut args = Vec::new();
    if has_params {
        let has_required = params.iter().any(|p| p.required && p.location != "header");
        let optional = if has_required { "" } else { "?" };
        args.push(format!("params{optional}: {params_type_name}"));
    }
    if let Some(bt) = body_type {
        args.push(format!("data: {bt}"));
    }
    args.push("options?: RequestInit".to_string());

    let args_str = args.join(", ");
    let return_type = if response_type == "void" {
        "Promise<void>".to_string()
    } else {
        format!("Promise<{response_type}>")
    };

    output.push_str(&format!(
        "export const {op_id} = async ({args_str}): {return_type} => {{\n"
    ));

    // Build URL
    let path_params: Vec<_> = params.iter().filter(|p| p.location == "path").collect();
    let query_params: Vec<_> = params.iter().filter(|p| p.location == "query").collect();

    if !path_params.is_empty() {
        // Path with parameters - use template literal
        let path_template = build_path_template(path);
        if !query_params.is_empty() {
            output.push_str(&format!("  const url = new URL(`{path_template}`, window.location.origin);\n"));
            for qp in &query_params {
                let optional_check = if qp.required { "" } else { "?" };
                output.push_str(&format!(
                    "  if (params{optional_check}.{} !== undefined) url.searchParams.set(\"{}\", String(params{optional_check}.{}));\n",
                    qp.name, qp.name, qp.name
                ));
            }
            output.push_str(&format!(
                "  const res = await fetch(url, {{ ...options, method: \"{method}\"",
            ));
        } else {
            output.push_str(&format!(
                "  const res = await fetch(`{path_template}`, {{ ...options, method: \"{method}\"",
            ));
        }
    } else if !query_params.is_empty() {
        output.push_str(&format!(
            "  const url = new URL(\"{path}\", window.location.origin);\n"
        ));
        for qp in &query_params {
            let optional_check = if qp.required { "" } else { "?" };
            output.push_str(&format!(
                "  if (params{optional_check}.{} !== undefined) url.searchParams.set(\"{}\", String(params{optional_check}.{}));\n",
                qp.name, qp.name, qp.name
            ));
        }
        output.push_str(&format!(
            "  const res = await fetch(url, {{ ...options, method: \"{method}\"",
        ));
    } else {
        output.push_str(&format!(
            "  const res = await fetch(\"{path}\", {{ ...options, method: \"{method}\"",
        ));
    }

    // Add body if needed
    if body_type.is_some() {
        output.push_str(", headers: { \"Content-Type\": \"application/json\", ...options?.headers }, body: JSON.stringify(data)");
    }

    output.push_str(" });\n");

    // Handle response
    output.push_str("  if (!res.ok) throw new Error(`HTTP ${res.status}`);\n");

    if response_type == "void" {
        output.push_str("  return;\n");
    } else {
        output.push_str("  return res.json();\n");
    }

    output.push_str("};\n\n");
    Ok(output)
}

/// Build a path template string with parameter interpolation.
fn build_path_template(path: &str) -> String {
    // Replace {paramName} with ${params.paramName}
    let mut result = String::new();
    let mut chars = path.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' {
            // Start of parameter
            let mut param_name = String::new();
            for inner in chars.by_ref() {
                if inner == '}' {
                    break;
                }
                param_name.push(inner);
            }
            result.push_str(&format!("${{params.{param_name}}}"));
        } else {
            result.push(c);
        }
    }

    result
}

/// Emit a query key function.
fn emit_query_key_function(
    op_id: &str,
    path: &str,
    has_params: bool,
    params_type_name: &str,
) -> String {
    let key_fn_name = format!("{op_id}Key");

    if has_params {
        format!(
            "export const {key_fn_name} = (params?: {params_type_name}) => [\"{path}\", params] as const;\n\n"
        )
    } else {
        format!("export const {key_fn_name} = () => [\"{path}\"] as const;\n\n")
    }
}

/// Emit a useQuery hook.
fn emit_use_query_hook(
    op_id: &str,
    response_type: &str,
    has_params: bool,
    params_type_name: &str,
) -> String {
    let hook_name = format!("use{}", capitalize_first(op_id));
    let key_fn_name = format!("{op_id}Key");

    if has_params {
        format!(
            r#"export function {hook_name}<TData = {response_type}>(
  params?: {params_type_name},
  options?: Omit<UseQueryOptions<{response_type}, Error, TData>, "queryKey" | "queryFn">
) {{
  return useQuery({{ queryKey: {key_fn_name}(params), queryFn: () => {op_id}(params), ...options }});
}}

"#
        )
    } else {
        format!(
            r#"export function {hook_name}<TData = {response_type}>(
  options?: Omit<UseQueryOptions<{response_type}, Error, TData>, "queryKey" | "queryFn">
) {{
  return useQuery({{ queryKey: {key_fn_name}(), queryFn: () => {op_id}(), ...options }});
}}

"#
        )
    }
}

/// Emit a useSuspenseQuery hook.
fn emit_use_suspense_query_hook(
    op_id: &str,
    response_type: &str,
    has_params: bool,
    params_type_name: &str,
) -> String {
    let hook_name = format!("use{}Suspense", capitalize_first(op_id));
    let key_fn_name = format!("{op_id}Key");

    if has_params {
        format!(
            r#"export function {hook_name}<TData = {response_type}>(
  params?: {params_type_name},
  options?: Omit<UseSuspenseQueryOptions<{response_type}, Error, TData>, "queryKey" | "queryFn">
) {{
  return useSuspenseQuery({{ queryKey: {key_fn_name}(params), queryFn: () => {op_id}(params), ...options }});
}}

"#
        )
    } else {
        format!(
            r#"export function {hook_name}<TData = {response_type}>(
  options?: Omit<UseSuspenseQueryOptions<{response_type}, Error, TData>, "queryKey" | "queryFn">
) {{
  return useSuspenseQuery({{ queryKey: {key_fn_name}(), queryFn: () => {op_id}(), ...options }});
}}

"#
        )
    }
}

/// Emit a useMutation hook.
fn emit_use_mutation_hook(
    op_id: &str,
    response_type: &str,
    body_type: Option<&str>,
    has_path_params: bool,
    params_type_name: &str,
) -> String {
    let hook_name = format!("use{}", capitalize_first(op_id));

    // Determine the mutation variables type
    let vars_type = match (has_path_params, body_type) {
        (true, Some(bt)) => format!("{{ params: {params_type_name}; data: {bt} }}"),
        (true, None) => format!("{{ params: {params_type_name} }}"),
        (false, Some(bt)) => bt.to_string(),
        (false, None) => "void".to_string(),
    };

    // Build the mutation function call
    let mutation_fn = match (has_path_params, body_type) {
        (true, Some(_)) => format!("(vars) => {op_id}(vars.params, vars.data)"),
        (true, None) => format!("(vars) => {op_id}(vars.params)"),
        (false, Some(_)) => format!("(data) => {op_id}(data)"),
        (false, None) => format!("() => {op_id}()"),
    };

    format!(
        r#"export function {hook_name}(
  options?: UseMutationOptions<{response_type}, Error, {vars_type}>
) {{
  return useMutation({{ mutationFn: {mutation_fn}, ...options }});
}}

"#
    )
}

/// Capitalize the first letter of a string.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}
