//! TypeScript code printer.
//!
//! This module prints the IR to TypeScript code strings.
//! It's intentionally "dumb" - no OpenAPI logic, just mechanical printing.

use super::api::{
    ApiIR, BodyContentType, FetchArgIR, FetchIR, HookIR, HookKind, OperationIR, ParamsIR,
    QueryKeyIR, ResponseContentType, UrlPart,
};
use super::types::{TsLiteral, TsPrimitive, TsType, TsTypeDef, TypeDefKind, TypeRef};
use super::utils::{escape_js_string, format_param_access, needs_bracket_notation, quote_if_needed};

/// Print a complete module
pub fn print_module(api: &ApiIR) -> String {
    let mut output = String::new();

    // Emit imports
    output.push_str(&print_imports(api.has_queries, api.has_mutations));

    // Emit types
    for type_def in &api.types {
        output.push_str(&print_type_def(type_def));
        output.push('\n');
    }

    // Emit operations
    for op in &api.operations {
        output.push_str(&print_operation(op));
    }

    output
}

/// Print imports and ApiError class
fn print_imports(has_queries: bool, has_mutations: bool) -> String {
    if !has_queries && !has_mutations {
        return String::new();
    }

    let mut runtime_imports = Vec::new();
    let mut type_imports = Vec::new();

    if has_queries {
        runtime_imports.push("useQuery");
        runtime_imports.push("useSuspenseQuery");
        type_imports.push("UseQueryOptions");
        type_imports.push("UseSuspenseQueryOptions");
    }
    if has_mutations {
        runtime_imports.push("useMutation");
        type_imports.push("UseMutationOptions");
    }

    let mut output = String::new();

    output.push_str(&format!(
        "import {{ {} }} from \"@tanstack/react-query\";\n",
        runtime_imports.join(", ")
    ));

    if !type_imports.is_empty() {
        output.push_str(&format!(
            "import type {{ {} }} from \"@tanstack/react-query\";\n",
            type_imports.join(", ")
        ));
    }

    output.push('\n');

    // Emit ApiError class for typed error handling
    output.push_str(
        r#"export class ApiError extends Error {
  status: number;
  statusText: string;
  body: unknown;

  constructor(status: number, statusText: string, body: unknown) {
    super(`HTTP ${status}: ${statusText}`);
    this.name = "ApiError";
    this.status = status;
    this.statusText = statusText;
    this.body = body;
  }
}

"#,
    );

    output
}

/// Print a type definition
fn print_type_def(type_def: &TsTypeDef) -> String {
    match &type_def.kind {
        TypeDefKind::Interface { properties } => {
            let mut output = format!("export interface {} {{\n", type_def.name);
            for prop in properties {
                let key = format_property_key(&prop.name);
                let opt = if prop.optional { "?" } else { "" };
                output.push_str(&format!("  {}{}: {};\n", key, opt, print_type(&prop.ty)));
            }
            output.push_str("}\n");
            output
        }
        TypeDefKind::TypeAlias { ty } => {
            format!("export type {} = {};\n", type_def.name, print_type(ty))
        }
        TypeDefKind::ConstEnum { values } => {
            let mut output = format!("export const {} = {{\n", type_def.name);
            for (key, value) in values {
                output.push_str(&format!("  {}: {},\n", key, print_literal(value)));
            }
            output.push_str("} as const;\n\n");
            output.push_str(&format!(
                "export type {} = (typeof {})[keyof typeof {}];\n",
                type_def.name, type_def.name, type_def.name
            ));
            output
        }
    }
}

/// Print a type
pub fn print_type(ty: &TsType) -> String {
    match ty {
        TsType::Primitive(p) => match p {
            TsPrimitive::String => "string".to_string(),
            TsPrimitive::Number => "number".to_string(),
            TsPrimitive::Boolean => "boolean".to_string(),
            TsPrimitive::Null => "null".to_string(),
            TsPrimitive::Void => "void".to_string(),
            TsPrimitive::Unknown => "unknown".to_string(),
        },
        TsType::Array(inner) => {
            let inner_str = print_type(inner);
            // Wrap complex types in parentheses
            if matches!(**inner, TsType::Union(_) | TsType::Intersection(_)) {
                format!("({})[]", inner_str)
            } else {
                format!("{}[]", inner_str)
            }
        }
        TsType::Union(types) => types.iter().map(print_type).collect::<Vec<_>>().join(" | "),
        TsType::Intersection(types) => {
            types
                .iter()
                .map(|t| {
                    let s = print_type(t);
                    if matches!(t, TsType::Union(_)) {
                        format!("({})", s)
                    } else {
                        s
                    }
                })
                .collect::<Vec<_>>()
                .join(" & ")
        }
        TsType::Object(props) => {
            if props.is_empty() {
                "{}".to_string()
            } else {
                let parts: Vec<_> = props
                    .iter()
                    .map(|p| {
                        let key = format_property_key(&p.name);
                        let opt = if p.optional { "?" } else { "" };
                        format!("{}{}: {}", key, opt, print_type(&p.ty))
                    })
                    .collect();
                format!("{{ {} }}", parts.join("; "))
            }
        }
        TsType::Record { key, value } => {
            format!("Record<{}, {}>", print_type(key), print_type(value))
        }
        TsType::Literal(lit) => print_literal(lit),
        TsType::Ref(name) => name.clone(),
    }
}

/// Print a literal value
fn print_literal(lit: &TsLiteral) -> String {
    match lit {
        TsLiteral::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{}\"", escaped)
        }
        TsLiteral::Number(n) => n.to_string(),
        TsLiteral::Int(i) => i.to_string(),
        TsLiteral::Bool(b) => b.to_string(),
        TsLiteral::Null => "null".to_string(),
    }
}

/// Format property key (quote if needed)
fn format_property_key(name: &str) -> String {
    quote_if_needed(name)
}

/// Convert a TypeRef to its TypeScript string representation.
fn type_ref_to_string(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named(n) => n.clone(),
        TypeRef::Inline(t) => print_type(t),
    }
}

/// Print an operation (params type + fetch + query key + hooks)
fn print_operation(op: &OperationIR) -> String {
    let mut output = String::new();

    // Emit params type if needed
    if let Some(params) = &op.params {
        output.push_str(&print_params_type(params));
    }

    // Emit fetch function
    output.push_str(&print_fetch_function(&op.fetch));

    // Emit query key function (for queries)
    if let Some(qk) = &op.query_key {
        output.push_str(&print_query_key_function(qk));
    }

    // Emit hooks
    for hook in &op.hooks {
        output.push_str(&print_hook(hook));
    }

    output
}

/// Print params type interface
fn print_params_type(params: &ParamsIR) -> String {
    let mut output = format!("export interface {} {{\n", params.type_name);

    for field in &params.fields {
        let key = format_property_key(&field.name);
        let opt = if field.required { "" } else { "?" };
        output.push_str(&format!("  {}{}: {};\n", key, opt, type_ref_to_string(&field.ty)));
    }

    output.push_str("}\n\n");
    output
}

/// Print fetch function
fn print_fetch_function(fetch: &FetchIR) -> String {
    let mut output = String::new();

    // Build argument list
    let mut args = Vec::new();
    let mut body_content_type = None;

    for arg in &fetch.args {
        match arg {
            FetchArgIR::Params { ty, optional } => {
                let opt = if *optional { "?" } else { "" };
                args.push(format!("params{}: {}", opt, type_ref_to_string(ty)));
            }
            FetchArgIR::Body { ty, content_type } => {
                body_content_type = Some(*content_type);
                let ty_str = match content_type {
                    BodyContentType::FormData => "FormData".to_string(),
                    BodyContentType::UrlEncoded | BodyContentType::Json => type_ref_to_string(ty),
                };
                args.push(format!("data: {}", ty_str));
            }
            FetchArgIR::Options => {
                args.push("options?: RequestInit".to_string());
            }
        }
    }

    // Return type - consider response content type and void status
    let response_type = type_ref_to_string(&fetch.response.ty);

    // Determine the actual TS return type based on content type
    let ts_response_type = match fetch.response.content_type {
        ResponseContentType::Text => "string".to_string(),
        ResponseContentType::Blob => "Blob".to_string(),
        ResponseContentType::Unknown => "Response".to_string(),
        ResponseContentType::Json => response_type.clone(),
    };

    let return_type = if ts_response_type == "void" {
        "Promise<void>".to_string()
    } else if fetch.response.has_void_status {
        // Union type for 204 + other status
        format!("Promise<{{ data: {} }} | void>", ts_response_type)
    } else {
        format!("Promise<{{ data: {} }}>", ts_response_type)
    };

    // Function signature
    output.push_str(&format!(
        "export const {} = async ({}): {} => {{\n",
        fetch.fn_name,
        args.join(", "),
        return_type
    ));

    // URL building
    let has_path_params = fetch
        .url
        .template
        .iter()
        .any(|p| matches!(p, UrlPart::Param(_)));
    let has_query_params = !fetch.url.query_params.is_empty();

    if has_path_params || has_query_params {
        let path_template = build_path_template_string(&fetch.url.template);

        if has_query_params {
            // Use URLSearchParams for query string building (no window.location.origin needed)
            output.push_str("  const searchParams = new URLSearchParams();\n");
            for qp in &fetch.url.query_params {
                let access = format_param_access("params", &qp.name, qp.required);

                // Check if param type is an array - use forEach with append for repeated params
                if qp.ty.is_array() {
                    output.push_str(&format!(
                        "  if ({} != null) {}.forEach((v) => searchParams.append(\"{}\", String(v)));\n",
                        access, access, qp.original_name
                    ));
                } else {
                    output.push_str(&format!(
                        "  if ({} != null) searchParams.set(\"{}\", String({}));\n",
                        access, qp.original_name, access
                    ));
                }
            }
            output.push_str(&format!(
                "  const queryString = searchParams.toString();\n  const url = queryString ? `{}?${{queryString}}` : `{}`;\n",
                path_template, path_template
            ));
            output.push_str(&format!(
                "  const res = await fetch(url, {{ ...options, method: \"{}\"",
                fetch.method.as_str()
            ));
        } else {
            // Just path params, use template literal directly
            output.push_str(&format!(
                "  const res = await fetch(`{}`, {{ ...options, method: \"{}\"",
                path_template,
                fetch.method.as_str()
            ));
        }
    } else {
        // No params at all
        let path = fetch
            .url
            .template
            .iter()
            .filter_map(|p| match p {
                UrlPart::Static(s) => Some(s.as_str()),
                UrlPart::Param(_) => None,
            })
            .collect::<Vec<_>>()
            .join("");
        output.push_str(&format!(
            "  const res = await fetch(\"{}\", {{ ...options, method: \"{}\"",
            path,
            fetch.method.as_str()
        ));
    }

    // Add headers (including header params) and body if needed
    let has_header_params = !fetch.header_params.is_empty();
    let has_body = fetch.body.is_some();

    if has_body || has_header_params {
        output.push_str(", headers: { ");

        // Add content-type header based on body content type
        if let Some(content_type) = body_content_type {
            match content_type {
                BodyContentType::Json => {
                    output.push_str("\"Content-Type\": \"application/json\", ");
                }
                BodyContentType::UrlEncoded => {
                    output.push_str(
                        "\"Content-Type\": \"application/x-www-form-urlencoded\", ",
                    );
                }
                BodyContentType::FormData => {
                    // Don't set Content-Type for FormData - browser sets it with boundary
                }
            }
        }

        // Add header params
        for hp in &fetch.header_params {
            if hp.required {
                // Required headers can be assigned directly
                let access = format_param_access("params", &hp.name, true);
                output.push_str(&format!("\"{}\": {}, ", hp.original_name, access));
            } else {
                // Optional headers use conditional spread to avoid undefined values
                let access = format_param_access("params", &hp.name, false);
                let direct_access = format_param_access("params", &hp.name, true);
                output.push_str(&format!(
                    "...({} != null && {{ \"{}\": {} }}), ",
                    access, hp.original_name, direct_access
                ));
            }
        }

        output.push_str("...options?.headers }");

        // Add body
        if has_body {
            match body_content_type {
                Some(BodyContentType::Json) => {
                    output.push_str(", body: JSON.stringify(data)");
                }
                Some(BodyContentType::UrlEncoded) => {
                    output.push_str(", body: new URLSearchParams(data as Record<string, string>)");
                }
                Some(BodyContentType::FormData) => {
                    output.push_str(", body: data");
                }
                None => {}
            }
        }
    }

    output.push_str(" });\n");

    // Error handling - try to parse error body as JSON, fallback to text
    output.push_str(
        r#"  if (!res.ok) {
    const body = await res.text();
    let parsed: unknown;
    try { parsed = JSON.parse(body); } catch { parsed = body; }
    throw new ApiError(res.status, res.statusText, parsed);
  }
"#,
    );

    // Return based on response content type and void status
    if ts_response_type == "void" {
        output.push_str("  return;\n");
    } else if fetch.response.has_void_status {
        // Check for 204 at runtime
        output.push_str("  if (res.status === 204) return;\n");
        output.push_str(&format!(
            "  return {{ data: await res.{}() }};\n",
            response_method_for_content_type(fetch.response.content_type)
        ));
    } else {
        output.push_str(&format!(
            "  return {{ data: await res.{}() }};\n",
            response_method_for_content_type(fetch.response.content_type)
        ));
    }

    output.push_str("};\n\n");
    output
}

/// Get the response method based on content type
fn response_method_for_content_type(content_type: ResponseContentType) -> &'static str {
    match content_type {
        ResponseContentType::Json => "json",
        ResponseContentType::Text => "text",
        ResponseContentType::Blob => "blob",
        ResponseContentType::Unknown => "json", // fallback, but Response is returned directly
    }
}

/// Build path template string
fn build_path_template_string(template: &[UrlPart]) -> String {
    template
        .iter()
        .map(|p| match p {
            UrlPart::Static(s) => s.clone(),
            UrlPart::Param(name) => {
                // Path params are always required, so we use the format_param_access
                // but wrap it in ${} for template literal interpolation
                if needs_bracket_notation(name) {
                    format!("${{params[\"{}\"]}}", escape_js_string(name))
                } else {
                    format!("${{params.{}}}", name)
                }
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Print query key function
fn print_query_key_function(qk: &QueryKeyIR) -> String {
    if let Some(params_type) = &qk.params_type {
        format!(
            "export const {} = (params?: {}) => [\"{}\", params] as const;\n\n",
            qk.fn_name,
            type_ref_to_string(params_type),
            qk.base_key
        )
    } else {
        format!(
            "export const {} = () => [\"{}\"] as const;\n\n",
            qk.fn_name, qk.base_key
        )
    }
}

/// Format the wrapped response type for hooks (e.g., `{ data: Item }` or `void`)
fn format_wrapped_response_type(response_str: &str) -> String {
    if response_str == "void" {
        "void".to_string()
    } else {
        format!("{{ data: {} }}", response_str)
    }
}

/// Print hook
fn print_hook(hook: &HookIR) -> String {
    let response_str = type_ref_to_string(&hook.response_type);
    let wrapped_type = format_wrapped_response_type(&response_str);

    match hook.kind {
        HookKind::Query | HookKind::SuspenseQuery => {
            let key_fn = hook.query_key_fn.as_ref().unwrap();
            let hook_fn = if hook.kind == HookKind::Query {
                "useQuery"
            } else {
                "useSuspenseQuery"
            };
            let options_type = if hook.kind == HookKind::Query {
                "UseQueryOptions"
            } else {
                "UseSuspenseQueryOptions"
            };

            if let Some(vars) = &hook.vars_type {
                format!(
                    r#"export function {}<TData = {}>(
  options?: {{ params?: {}; query?: Omit<{}<{}, ApiError, TData>, "queryKey" | "queryFn"> }}
) {{
  return {}({{ queryKey: {}(options?.params), queryFn: () => {}(options?.params), ...options?.query }});
}}

"#,
                    hook.name,
                    wrapped_type,
                    type_ref_to_string(vars),
                    options_type,
                    wrapped_type,
                    hook_fn,
                    key_fn,
                    hook.fetch_fn
                )
            } else {
                format!(
                    r#"export function {}<TData = {}>(
  options?: {{ query?: Omit<{}<{}, ApiError, TData>, "queryKey" | "queryFn"> }}
) {{
  return {}({{ queryKey: {}(), queryFn: () => {}(), ...options?.query }});
}}

"#,
                    hook.name, wrapped_type, options_type, wrapped_type, hook_fn, key_fn,
                    hook.fetch_fn
                )
            }
        }
        HookKind::Mutation => {
            let vars_str = hook
                .vars_type
                .as_ref()
                .map(type_ref_to_string)
                .unwrap_or_else(|| "void".to_string());

            // Build mutation function call
            let mutation_fn = if let Some(vars) = &hook.vars_type {
                match vars {
                    TypeRef::Inline(t) => match &**t {
                        TsType::Object(props) => {
                            let has_params = props.iter().any(|p| p.name == "params");
                            let has_data = props.iter().any(|p| p.name == "data");
                            match (has_params, has_data) {
                                (true, true) => {
                                    format!("(vars) => {}(vars.params, vars.data)", hook.fetch_fn)
                                }
                                (true, false) => {
                                    format!("(vars) => {}(vars.params)", hook.fetch_fn)
                                }
                                _ => format!("(data) => {}(data)", hook.fetch_fn),
                            }
                        }
                        _ => format!("(data) => {}(data)", hook.fetch_fn),
                    },
                    TypeRef::Named(_) => format!("(data) => {}(data)", hook.fetch_fn),
                }
            } else {
                format!("() => {}()", hook.fetch_fn)
            };

            format!(
                r#"export function {}(
  options?: {{ mutation?: UseMutationOptions<{}, ApiError, {}> }}
) {{
  return useMutation({{ mutationFn: {}, ...options?.mutation }});
}}

"#,
                hook.name, wrapped_type, vars_str, mutation_fn
            )
        }
    }
}
