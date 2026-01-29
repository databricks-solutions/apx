//! Normalization from OpenAPI spec to API IR.
//!
//! This module handles all the OpenAPI-specific logic:
//! - Schema to TypeScript type conversion
//! - Parameter merging and deduplication
//! - Operation normalization

use std::collections::HashMap;

use crate::openapi::spec::{
    AdditionalProperties, Discriminator, EnumValue, OpenApiSpec, Operation, Parameter, Schema,
    SchemaType,
};

use super::api::{
    ApiIR, BodyContentType, BodyIR, FetchArgIR, FetchIR, HookIR, HookKind, HttpMethod, OperationIR,
    OperationKind, ParamIR, ParamLocation, ParamsIR, QueryKeyIR, ResponseContentType, ResponseIR,
    UrlIR, UrlPart,
};
use super::types::{TsLiteral, TsPrimitive, TsProp, TsType, TsTypeDef, TypeDefKind, TypeRef};
use super::utils::{
    capitalize_first, enum_value_to_key, enum_value_to_literal, make_string_record,
    make_unknown_record, sanitize_ts_identifier, to_snake_case,
};

/// Helper to process a single HTTP method operation
fn process_operation(
    path: &str,
    method: HttpMethod,
    op: Option<&Operation>,
    path_params: Option<&Vec<Parameter>>,
    operations: &mut Vec<OperationIR>,
    operation_names: &mut std::collections::HashSet<String>,
) -> Result<bool, String> {
    if let Some(op) = op {
        let op_ir = normalize_operation(path, method, op, path_params)?;

        // Check for operationId collision
        if !operation_names.insert(op_ir.name.clone()) {
            return Err(format!(
                "Duplicate operationId '{}' detected. Each operation must have a unique identifier.",
                op_ir.name
            ));
        }

        operations.push(op_ir);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Normalize an OpenAPI spec into API IR
pub fn normalize_spec(spec: &OpenApiSpec) -> Result<ApiIR, String> {
    let mut operations = Vec::new();
    let mut has_queries = false;
    let mut has_mutations = false;
    let mut operation_names = std::collections::HashSet::new();

    // Sort paths for deterministic output
    let mut paths: Vec<_> = spec.paths.iter().collect();
    paths.sort_by_key(|(path, _)| *path);

    for (path, item) in paths {
        let path_params = item.parameters.as_ref();

        // Process each HTTP method
        if process_operation(
            path,
            HttpMethod::Get,
            item.get.as_ref(),
            path_params,
            &mut operations,
            &mut operation_names,
        )? {
            has_queries = true;
        }

        // POST, PUT, PATCH, DELETE are mutations
        for (method, op) in [
            (HttpMethod::Post, item.post.as_ref()),
            (HttpMethod::Put, item.put.as_ref()),
            (HttpMethod::Patch, item.patch.as_ref()),
            (HttpMethod::Delete, item.delete.as_ref()),
        ] {
            if process_operation(
                path,
                method,
                op,
                path_params,
                &mut operations,
                &mut operation_names,
            )? {
                has_mutations = true;
            }
        }
    }

    // Normalize component schemas
    let types = if let Some(components) = &spec.components {
        if let Some(schemas) = &components.schemas {
            normalize_schemas(schemas)?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Ok(ApiIR {
        operations,
        types,
        has_queries,
        has_mutations,
    })
}

/// Normalize component schemas into type definitions
fn normalize_schemas(schemas: &HashMap<String, Schema>) -> Result<Vec<TsTypeDef>, String> {
    let mut type_defs = Vec::new();

    // Sort for deterministic output
    let mut names: Vec<_> = schemas.keys().collect();
    names.sort();

    for name in names {
        let Some(schema) = schemas.get(name) else {
            continue;
        };
        let type_def = normalize_schema_to_typedef(name, schema)?;
        type_defs.push(type_def);
    }

    Ok(type_defs)
}

/// Convert a schema to a type definition
fn normalize_schema_to_typedef(name: &str, schema: &Schema) -> Result<TsTypeDef, String> {
    // Check for enum
    if let Some(enum_values) = &schema.enum_values {
        return Ok(TsTypeDef {
            name: name.to_string(),
            kind: TypeDefKind::ConstEnum {
                values: normalize_enum_values(enum_values),
            },
        });
    }

    // Check for plain object (interface candidate)
    if schema.properties.is_some() && schema.additional_properties.is_none() {
        if let Some(properties) = &schema.properties {
            let props = normalize_properties(properties, schema.required.as_ref())?;
            return Ok(TsTypeDef {
                name: name.to_string(),
                kind: TypeDefKind::Interface { properties: props },
            });
        }
    }

    // Everything else becomes a type alias
    let ty = schema_to_ts_type(schema)?;
    Ok(TsTypeDef {
        name: name.to_string(),
        kind: TypeDefKind::TypeAlias { ty },
    })
}

/// Normalize enum values to (key, literal) pairs for const enum objects
fn normalize_enum_values(values: &[EnumValue]) -> Vec<(String, TsLiteral)> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| (enum_value_to_key(v, i), enum_value_to_literal(v)))
        .collect()
}

/// Normalize object properties
fn normalize_properties(
    properties: &HashMap<String, Schema>,
    required: Option<&Vec<String>>,
) -> Result<Vec<TsProp>, String> {
    let required_set: std::collections::HashSet<_> =
        required.map(|r| r.iter().collect()).unwrap_or_default();

    let mut props = Vec::new();
    let mut names: Vec<_> = properties.keys().collect();
    names.sort();

    for name in names {
        let Some(schema) = properties.get(name) else {
            continue;
        };
        let ty = schema_to_ts_type(schema)?;
        props.push(TsProp {
            name: name.clone(),
            ty,
            optional: !required_set.contains(name),
        });
    }

    Ok(props)
}

/// Convert a Schema to TsType
pub fn schema_to_ts_type(schema: &Schema) -> Result<TsType, String> {
    // Handle $ref first
    if let Some(ref_path) = &schema.ref_path {
        return Ok(TsType::Ref(ref_to_type_name(ref_path)));
    }

    // Handle const keyword
    if let Some(const_value) = &schema.const_value {
        return Ok(json_value_to_ts_type(const_value));
    }

    // Handle allOf (intersection)
    if let Some(all_of) = &schema.all_of {
        return normalize_intersection(all_of);
    }

    // Handle anyOf (union, often nullable)
    if let Some(any_of) = &schema.any_of {
        return normalize_union(any_of, None);
    }

    // Handle oneOf (discriminated union)
    if let Some(one_of) = &schema.one_of {
        return normalize_union(one_of, schema.discriminator.as_ref());
    }

    // Handle type
    match &schema.schema_type {
        Some(SchemaType::Single(t)) => schema_type_to_ts(t, schema),
        Some(SchemaType::Multiple(types)) => {
            let non_null: Vec<_> = types.iter().filter(|t| *t != "null").collect();
            if non_null.len() == 1 {
                let base_type = schema_type_to_ts(non_null[0], schema)?;
                if types.contains(&"null".to_string()) {
                    Ok(TsType::Union(vec![
                        base_type,
                        TsType::Primitive(TsPrimitive::Null),
                    ]))
                } else {
                    Ok(base_type)
                }
            } else {
                let mut ts_types: Vec<_> = non_null
                    .iter()
                    .map(|t| schema_type_to_ts(t, schema))
                    .collect::<Result<Vec<_>, _>>()?;
                if types.contains(&"null".to_string()) {
                    ts_types.push(TsType::Primitive(TsPrimitive::Null));
                }
                Ok(TsType::Union(ts_types))
            }
        }
        None => {
            // No type specified - check for additionalProperties or default to unknown
            if schema.additional_properties.is_some() {
                normalize_additional_properties(schema)
            } else {
                Ok(TsType::Primitive(TsPrimitive::Unknown))
            }
        }
    }
}

/// Convert JSON value to TsType literal
fn json_value_to_ts_type(value: &serde_json::Value) -> TsType {
    match value {
        serde_json::Value::Null => TsType::Literal(TsLiteral::Null),
        serde_json::Value::Bool(b) => TsType::Literal(TsLiteral::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                TsType::Literal(TsLiteral::Int(i))
            } else {
                TsType::Literal(TsLiteral::Number(n.as_f64().unwrap_or(0.0)))
            }
        }
        serde_json::Value::String(s) => TsType::Literal(TsLiteral::String(s.clone())),
        _ => TsType::Primitive(TsPrimitive::Unknown),
    }
}

/// Normalize intersection type (allOf)
fn normalize_intersection(schemas: &[Schema]) -> Result<TsType, String> {
    let mut types: Vec<_> = schemas
        .iter()
        .map(schema_to_ts_type)
        .collect::<Result<Vec<_>, _>>()?;

    if types.is_empty() {
        return Ok(TsType::Primitive(TsPrimitive::Unknown));
    }
    if types.len() == 1 {
        if let Some(ty) = types.pop() {
            return Ok(ty);
        }
    }

    Ok(TsType::Intersection(types))
}

/// Normalize union type (anyOf/oneOf)
fn normalize_union(
    schemas: &[Schema],
    discriminator: Option<&Discriminator>,
) -> Result<TsType, String> {
    // With discriminator, create discriminated union
    if let Some(disc) = discriminator {
        let mut union_types = Vec::new();
        for schema in schemas {
            let base_type = schema_to_ts_type(schema)?;

            // Determine discriminator value
            let disc_value = if let Some(mapping) = &disc.mapping {
                if let Some(ref_path) = &schema.ref_path {
                    mapping
                        .iter()
                        .find(|(_, v)| *v == ref_path)
                        .map(|(k, _)| k.clone())
                } else {
                    None
                }
            } else {
                schema
                    .ref_path
                    .as_ref()
                    .map(|ref_path| ref_to_type_name(ref_path))
            };

            if let Some(value) = disc_value {
                // Create intersection: { petType: "dog" } & Dog
                let disc_prop = TsProp {
                    name: disc.property_name.clone(),
                    ty: TsType::Literal(TsLiteral::String(value)),
                    optional: false,
                };
                union_types.push(TsType::Intersection(vec![
                    TsType::Object(vec![disc_prop]),
                    base_type,
                ]));
            } else {
                union_types.push(base_type);
            }
        }
        return Ok(TsType::Union(union_types));
    }

    // Standard union
    let types: Vec<_> = schemas
        .iter()
        .map(schema_to_ts_type)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TsType::Union(types))
}

/// Convert single schema type to TsType
fn schema_type_to_ts(schema_type: &str, schema: &Schema) -> Result<TsType, String> {
    match schema_type {
        "string" => {
            if let Some(enum_values) = &schema.enum_values {
                Ok(enum_to_union_type(enum_values))
            } else {
                Ok(TsType::Primitive(TsPrimitive::String))
            }
        }
        "number" | "integer" => {
            if let Some(enum_values) = &schema.enum_values {
                Ok(enum_to_union_type(enum_values))
            } else {
                Ok(TsType::Primitive(TsPrimitive::Number))
            }
        }
        "boolean" => Ok(TsType::Primitive(TsPrimitive::Boolean)),
        "null" => Ok(TsType::Primitive(TsPrimitive::Null)),
        "array" => {
            if let Some(items) = &schema.items {
                let item_type = schema_to_ts_type(items)?;
                Ok(TsType::Array(Box::new(item_type)))
            } else {
                Ok(TsType::Array(Box::new(TsType::Primitive(
                    TsPrimitive::Unknown,
                ))))
            }
        }
        "object" => normalize_object_type(schema),
        _ => Ok(TsType::Primitive(TsPrimitive::Unknown)),
    }
}

/// Convert enum values to union of literal types
fn enum_to_union_type(values: &[EnumValue]) -> TsType {
    let types: Vec<_> = values
        .iter()
        .map(|v| TsType::Literal(enum_value_to_literal(v)))
        .collect();
    TsType::Union(types)
}

/// Normalize object type
fn normalize_object_type(schema: &Schema) -> Result<TsType, String> {
    let has_properties = schema.properties.is_some();
    let has_additional = schema.additional_properties.is_some();

    match (has_properties, has_additional) {
        (true, true) => {
            // Intersection of props and record
            let Some(properties) = schema.properties.as_ref() else {
                return Ok(make_unknown_record());
            };
            let props = normalize_properties(properties, schema.required.as_ref())?;
            let additional_type = normalize_additional_properties(schema)?;
            Ok(TsType::Intersection(vec![
                TsType::Object(props),
                additional_type,
            ]))
        }
        (true, false) => {
            let Some(properties) = schema.properties.as_ref() else {
                return Ok(make_unknown_record());
            };
            let props = normalize_properties(properties, schema.required.as_ref())?;
            Ok(TsType::Object(props))
        }
        (false, true) => normalize_additional_properties(schema),
        (false, false) => Ok(make_unknown_record()),
    }
}

/// Normalize additional properties to a Record type
fn normalize_additional_properties(schema: &Schema) -> Result<TsType, String> {
    match &schema.additional_properties {
        Some(AdditionalProperties::Bool(true)) | None => Ok(make_unknown_record()),
        Some(AdditionalProperties::Bool(false)) => Ok(TsType::Object(Vec::new())),
        Some(AdditionalProperties::Schema(s)) => {
            let value_type = schema_to_ts_type(s)?;
            Ok(make_string_record(value_type))
        }
    }
}

/// Extract type name from $ref path
fn ref_to_type_name(ref_path: &str) -> String {
    ref_path
        .strip_prefix("#/components/schemas/")
        .unwrap_or(ref_path)
        .to_string()
}

/// Normalize an operation
fn normalize_operation(
    path: &str,
    method: HttpMethod,
    op: &Operation,
    path_params: Option<&Vec<Parameter>>,
) -> Result<OperationIR, String> {
    let name = get_operation_name(path, method, op);
    let kind = if method.is_query() {
        OperationKind::Query
    } else {
        OperationKind::Mutation
    };

    // Normalize parameters
    let params = normalize_params(&name, op, path_params)?;

    // Normalize body
    let body = normalize_body(op)?;

    // Normalize response
    let response = normalize_response(op)?;

    // Build fetch IR
    let fetch = build_fetch_ir(&name, path, method, &params, &body, &response);

    // Build query key (for queries only)
    let query_key = if kind == OperationKind::Query {
        Some(build_query_key_ir(&name, path, &params))
    } else {
        None
    };

    // Build hooks
    let hooks = build_hooks(&name, kind, &params, &body, &response, &query_key);

    Ok(OperationIR {
        name,
        kind,
        path: path.to_string(),
        method,
        params,
        body,
        response,
        fetch,
        hooks,
        query_key,
    })
}

/// Get operation name
fn get_operation_name(path: &str, method: HttpMethod, op: &Operation) -> String {
    if let Some(id) = &op.operation_id {
        return sanitize_ts_identifier(id);
    }

    // Generate from path and method
    let path_parts: Vec<_> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect();

    let base = path_parts.join("_");
    sanitize_ts_identifier(&format!("{}_{}", method.as_str().to_lowercase(), base))
}

/// Check for duplicate parameter names within a list
fn check_duplicate_params(params: &[Parameter], location: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for p in params {
        // Skip cookie params as they're not included
        if p.location == "cookie" {
            continue;
        }
        if !seen.insert(&p.name) {
            return Err(format!(
                "Duplicate parameter name '{}' in {} parameters",
                p.name, location
            ));
        }
    }
    Ok(())
}

/// Normalize parameters - includes path, query, and header params; skips cookie params
fn normalize_params(
    op_name: &str,
    op: &Operation,
    path_params: Option<&Vec<Parameter>>,
) -> Result<Option<ParamsIR>, String> {
    let mut fields = Vec::new();

    // Check for duplicates within path-level params
    if let Some(pp) = path_params {
        check_duplicate_params(pp, "path-level")?;
        for p in pp {
            // Skip cookie params
            if p.location == "cookie" {
                continue;
            }
            fields.push(normalize_param(p));
        }
    }

    // Check for duplicates within operation-level params
    if let Some(op_params) = &op.parameters {
        check_duplicate_params(op_params, "operation-level")?;
        for p in op_params {
            // Skip cookie params
            if p.location == "cookie" {
                continue;
            }
            // Remove any existing param with same name (op-level overrides path-level)
            fields.retain(|f: &ParamIR| f.original_name != p.name);
            fields.push(normalize_param(p));
        }
    }

    if fields.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ParamsIR {
            type_name: format!("{}Params", capitalize_first(op_name)),
            fields,
        }))
    }
}

/// Normalize a single parameter
fn normalize_param(p: &Parameter) -> ParamIR {
    let ty = p
        .schema
        .as_ref()
        .and_then(|s| schema_to_ts_type(s).ok())
        .map(|t| TypeRef::Inline(Box::new(t)))
        .unwrap_or(TypeRef::Inline(Box::new(TsType::Primitive(
            TsPrimitive::String,
        ))));

    let location = match p.location.as_str() {
        "path" => ParamLocation::Path,
        "header" => ParamLocation::Header,
        _ => ParamLocation::Query,
    };

    ParamIR {
        name: p.name.clone(),
        original_name: p.name.clone(),
        ty,
        required: p.required,
        location,
    }
}

/// Normalize request body - detects content type and returns BodyIR
fn normalize_body(op: &Operation) -> Result<Option<BodyIR>, String> {
    if let Some(body) = &op.request_body {
        if let Some(content) = &body.content {
            // Check for multipart/form-data first
            if let Some(media_type) = content.get("multipart/form-data") {
                if let Some(schema) = &media_type.schema {
                    let ty = schema_to_ts_type(schema)?;
                    return Ok(Some(BodyIR {
                        ty: TypeRef::Inline(Box::new(ty)),
                        content_type: BodyContentType::FormData,
                    }));
                }
            }

            // Check for application/x-www-form-urlencoded
            if let Some(media_type) = content.get("application/x-www-form-urlencoded") {
                if let Some(schema) = &media_type.schema {
                    let ty = schema_to_ts_type(schema)?;
                    return Ok(Some(BodyIR {
                        ty: TypeRef::Inline(Box::new(ty)),
                        content_type: BodyContentType::UrlEncoded,
                    }));
                }
            }

            // Check for application/json
            if let Some(media_type) = content.get("application/json") {
                if let Some(schema) = &media_type.schema {
                    let ty = schema_to_ts_type(schema)?;
                    return Ok(Some(BodyIR {
                        ty: TypeRef::Inline(Box::new(ty)),
                        content_type: BodyContentType::Json,
                    }));
                }
            }
        }
    }
    Ok(None)
}

/// Determine response content type from media type string
fn detect_response_content_type(media_type: &str) -> ResponseContentType {
    if media_type == "application/json" || media_type.ends_with("+json") {
        ResponseContentType::Json
    } else if media_type == "text/plain"
        || media_type.starts_with("text/")
        || media_type == "application/xml"
        || media_type.ends_with("+xml")
    {
        ResponseContentType::Text
    } else if media_type == "application/octet-stream"
        || media_type.starts_with("image/")
        || media_type.starts_with("audio/")
        || media_type.starts_with("video/")
        || media_type == "application/pdf"
    {
        ResponseContentType::Blob
    } else {
        ResponseContentType::Unknown
    }
}

/// Normalize response - handles all 2XX codes, default, wildcards, and content types
fn normalize_response(op: &Operation) -> Result<ResponseIR, String> {
    // Check if 204 (No Content) exists
    let has_void_status = op.responses.contains_key("204");

    // Priority order for success responses: 200, 201, 202, 203, 206, 207, then default, then 2XX
    let status_codes = ["200", "201", "202", "203", "206", "207", "default", "2XX"];

    for status in status_codes {
        if let Some(response) = op.responses.get(status) {
            if let Some(content) = &response.content {
                // Find the first content type that has a schema
                for (media_type_str, media_type) in content {
                    if let Some(schema) = &media_type.schema {
                        let ty = schema_to_ts_type(schema)?;
                        let content_type = detect_response_content_type(media_type_str);

                        return Ok(ResponseIR {
                            ty: TypeRef::Inline(Box::new(ty)),
                            content_type,
                            has_void_status,
                        });
                    }
                }
            }
        }
    }

    // If we only have 204 and no other content response, return void
    if has_void_status {
        return Ok(ResponseIR {
            ty: TypeRef::Inline(Box::new(TsType::Primitive(TsPrimitive::Void))),
            content_type: ResponseContentType::Json, // doesn't matter for void
            has_void_status: false,                  // no need to check, it's always void
        });
    }

    // Default to unknown
    Ok(ResponseIR {
        ty: TypeRef::Inline(Box::new(TsType::Primitive(TsPrimitive::Unknown))),
        content_type: ResponseContentType::Unknown,
        has_void_status: false,
    })
}

/// Build fetch function IR
fn build_fetch_ir(
    name: &str,
    path: &str,
    method: HttpMethod,
    params: &Option<ParamsIR>,
    body: &Option<BodyIR>,
    response: &ResponseIR,
) -> FetchIR {
    let mut args = Vec::new();

    // Add params argument if needed
    if let Some(p) = params {
        let has_required = p.fields.iter().any(|f| f.required);
        args.push(FetchArgIR::Params {
            ty: TypeRef::Named(p.type_name.clone()),
            optional: !has_required,
        });
    }

    // Add body argument if needed
    if let Some(b) = body {
        args.push(FetchArgIR::Body {
            ty: b.ty.clone(),
            content_type: b.content_type,
        });
    }

    // Always add options
    args.push(FetchArgIR::Options);

    // Build URL IR
    let url = build_url_ir(path, params);

    // Collect header params
    let header_params = params
        .as_ref()
        .map(|p| {
            p.fields
                .iter()
                .filter(|f| f.location == ParamLocation::Header)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    FetchIR {
        fn_name: name.to_string(),
        args,
        response: response.clone(),
        url,
        body: body.clone(),
        method,
        header_params,
    }
}

/// Build URL IR
fn build_url_ir(path: &str, params: &Option<ParamsIR>) -> UrlIR {
    // Collect path params for lookup
    let path_params: Vec<&ParamIR> = params
        .as_ref()
        .map(|p| {
            p.fields
                .iter()
                .filter(|f| f.location == ParamLocation::Path)
                .collect()
        })
        .unwrap_or_default();

    // Parse path template
    let mut template = Vec::new();
    let mut current = String::new();
    let mut in_param = false;
    let mut placeholder_name = String::new();

    for c in path.chars() {
        if c == '{' {
            if !current.is_empty() {
                template.push(UrlPart::Static(current.clone()));
                current.clear();
            }
            in_param = true;
            placeholder_name.clear();
        } else if c == '}' {
            // Look up the actual param name from the spec
            // The placeholder in the path (e.g., "item_id") may differ from the param name (e.g., "itemId")
            let param_name = find_matching_param(&placeholder_name, &path_params)
                .unwrap_or_else(|| placeholder_name.clone());
            template.push(UrlPart::Param(param_name));
            in_param = false;
        } else if in_param {
            placeholder_name.push(c);
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        template.push(UrlPart::Static(current));
    }

    // Collect query params
    let query_params = params
        .as_ref()
        .map(|p| {
            p.fields
                .iter()
                .filter(|f| f.location == ParamLocation::Query)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    UrlIR {
        template,
        query_params,
    }
}

/// Find a matching param by trying exact match, then snake_case equivalence
fn find_matching_param(placeholder: &str, params: &[&ParamIR]) -> Option<String> {
    // Try exact match on original_name first
    for p in params {
        if p.original_name == placeholder {
            return Some(p.name.clone());
        }
    }

    // Try snake_case equivalence (handles camelCase param names like "itemId" matching "item_id")
    let placeholder_snake = to_snake_case(placeholder);
    for p in params {
        if to_snake_case(&p.original_name) == placeholder_snake {
            return Some(p.name.clone());
        }
    }

    None
}

/// Build query key IR
fn build_query_key_ir(name: &str, path: &str, params: &Option<ParamsIR>) -> QueryKeyIR {
    QueryKeyIR {
        fn_name: format!("{name}Key"),
        base_key: path.to_string(),
        params_type: params.as_ref().map(|p| TypeRef::Named(p.type_name.clone())),
    }
}

/// Build hooks for an operation
fn build_hooks(
    name: &str,
    kind: OperationKind,
    params: &Option<ParamsIR>,
    body: &Option<BodyIR>,
    response: &ResponseIR,
    query_key: &Option<QueryKeyIR>,
) -> Vec<HookIR> {
    let mut hooks = Vec::new();
    let capitalized = capitalize_first(name);

    match kind {
        OperationKind::Query => {
            let vars_type = params.as_ref().map(|p| TypeRef::Named(p.type_name.clone()));
            let key_fn = query_key.as_ref().map(|k| k.fn_name.clone());

            // useQuery
            hooks.push(HookIR {
                name: format!("use{capitalized}"),
                kind: HookKind::Query,
                response_type: response.ty.clone(),
                vars_type: vars_type.clone(),
                fetch_fn: name.to_string(),
                query_key_fn: key_fn.clone(),
            });

            // useSuspenseQuery
            hooks.push(HookIR {
                name: format!("use{capitalized}Suspense"),
                kind: HookKind::SuspenseQuery,
                response_type: response.ty.clone(),
                vars_type,
                fetch_fn: name.to_string(),
                query_key_fn: key_fn,
            });
        }
        OperationKind::Mutation => {
            // Determine vars type
            let vars_type = match (params.as_ref(), body.as_ref()) {
                (Some(p), Some(b)) => Some(TypeRef::Inline(Box::new(TsType::Object(vec![
                    TsProp {
                        name: "params".to_string(),
                        ty: TsType::Ref(p.type_name.clone()),
                        optional: false,
                    },
                    TsProp {
                        name: "data".to_string(),
                        ty: b.ty.to_ts_type(),
                        optional: false,
                    },
                ])))),
                (Some(p), None) => Some(TypeRef::Inline(Box::new(TsType::Object(vec![TsProp {
                    name: "params".to_string(),
                    ty: TsType::Ref(p.type_name.clone()),
                    optional: false,
                }])))),
                (None, Some(b)) => Some(b.ty.clone()),
                (None, None) => None,
            };

            hooks.push(HookIR {
                name: format!("use{capitalized}"),
                kind: HookKind::Mutation,
                response_type: response.ty.clone(),
                vars_type,
                fetch_fn: name.to_string(),
                query_key_fn: None,
            });
        }
    }

    hooks
}
