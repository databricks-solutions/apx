//! Code generation from Domain IR to TypeScript AST.
//!
//! This module transforms the high-level API IR (operations, hooks, params)
//! into TypeScript AST nodes (functions, statements, expressions).
//!
//! The generated AST can then be emitted to strings via the `Emit` trait.

use super::api::{
    ApiIR, BodyContentType, FetchArgIR, FetchIR, HookIR, HookKind, OperationIR, ParamsIR,
    QueryKeyIR, ResponseContentType, UrlPart,
};
use super::emit::Emit;
use super::types::{
    ImportItem, TsExpr, TsFunction, TsImport, TsLiteral, TsModule, TsParam, TsProp, TsStmt,
    TsType, TsTypeDef, TypeDefKind, TypeRef, VarKind,
};
use super::utils::{escape_js_string, format_param_access, needs_bracket_notation};

/// Generate a complete TypeScript module from API IR.
pub fn codegen_module(api: &ApiIR) -> TsModule {
    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut functions = Vec::new();

    // Generate imports
    if api.has_queries || api.has_mutations {
        imports.extend(codegen_imports(api.has_queries, api.has_mutations));

        // Generate ApiError class as a raw function (it's actually a class)
        functions.push(codegen_api_error_class());
    }

    // Add component schema types
    types.extend(api.types.iter().cloned());

    // Generate operations
    for op in &api.operations {
        let (op_types, op_funcs) = codegen_operation(op);
        types.extend(op_types);
        functions.extend(op_funcs);
    }

    TsModule {
        imports,
        types,
        functions,
    }
}

/// Generate import statements.
fn codegen_imports(has_queries: bool, has_mutations: bool) -> Vec<TsImport> {
    let mut imports = Vec::new();

    let mut runtime_items = Vec::new();
    let mut type_items = Vec::new();

    if has_queries {
        runtime_items.push(ImportItem {
            name: "useQuery".into(),
            alias: None,
        });
        runtime_items.push(ImportItem {
            name: "useSuspenseQuery".into(),
            alias: None,
        });
        type_items.push(ImportItem {
            name: "UseQueryOptions".into(),
            alias: None,
        });
        type_items.push(ImportItem {
            name: "UseSuspenseQueryOptions".into(),
            alias: None,
        });
    }

    if has_mutations {
        runtime_items.push(ImportItem {
            name: "useMutation".into(),
            alias: None,
        });
        type_items.push(ImportItem {
            name: "UseMutationOptions".into(),
            alias: None,
        });
    }

    if !runtime_items.is_empty() {
        imports.push(TsImport {
            items: runtime_items,
            from: "@tanstack/react-query".into(),
            type_only: false,
        });
    }

    if !type_items.is_empty() {
        imports.push(TsImport {
            items: type_items,
            from: "@tanstack/react-query".into(),
            type_only: true,
        });
    }

    imports
}

/// Generate the ApiError class.
/// Since our AST doesn't have class support, we use a raw function that outputs the class.
fn codegen_api_error_class() -> TsFunction {
    TsFunction {
        name: "".into(), // Empty name signals this is a raw block
        type_params: vec![],
        params: vec![],
        return_type: None,
        body: vec![TsStmt::Raw(
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
}"#
            .into(),
        )],
        is_async: false,
        is_export: false,
        is_arrow: false,
    }
}

/// Generate code for a single operation.
/// Returns (type_definitions, functions).
fn codegen_operation(op: &OperationIR) -> (Vec<TsTypeDef>, Vec<TsFunction>) {
    let mut types = Vec::new();
    let mut functions = Vec::new();

    // Generate params type if needed
    if let Some(params) = &op.params {
        types.push(codegen_params_type(params));
    }

    // Generate fetch function
    functions.push(codegen_fetch_function(&op.fetch));

    // Generate query key function (for queries)
    if let Some(qk) = &op.query_key {
        functions.push(codegen_query_key_function(qk));
    }

    // Generate hooks
    for hook in &op.hooks {
        functions.push(codegen_hook(hook));
    }

    (types, functions)
}

/// Generate a params interface type.
fn codegen_params_type(params: &ParamsIR) -> TsTypeDef {
    let properties = params
        .fields
        .iter()
        .map(|field| TsProp {
            name: field.name.clone(),
            ty: field.ty.to_ts_type(),
            optional: !field.required,
        })
        .collect();

    TsTypeDef {
        name: params.type_name.clone(),
        kind: TypeDefKind::Interface { properties },
    }
}

/// Generate a fetch function.
fn codegen_fetch_function(fetch: &FetchIR) -> TsFunction {
    let mut params = Vec::new();
    let mut body_content_type = None;

    // Build parameters
    for arg in &fetch.args {
        match arg {
            FetchArgIR::Params { ty, optional } => {
                params.push(TsParam {
                    name: "params".into(),
                    ty: Some(ty.to_ts_type()),
                    optional: *optional,
                });
            }
            FetchArgIR::Body { ty, content_type } => {
                body_content_type = Some(*content_type);
                let ty_type = match content_type {
                    BodyContentType::FormData => TsType::Ref("FormData".into()),
                    BodyContentType::UrlEncoded | BodyContentType::Json => ty.to_ts_type(),
                };
                params.push(TsParam {
                    name: "data".into(),
                    ty: Some(ty_type),
                    optional: false,
                });
            }
            FetchArgIR::Options => {
                params.push(TsParam {
                    name: "options".into(),
                    ty: Some(TsType::Ref("RequestInit".into())),
                    optional: true,
                });
            }
        }
    }

    // Build return type
    let response_type_str = fetch.response.ty.emit();
    let ts_response_type = match fetch.response.content_type {
        ResponseContentType::Text => "string".to_string(),
        ResponseContentType::Blob => "Blob".to_string(),
        ResponseContentType::Unknown => "Response".to_string(),
        ResponseContentType::Json => response_type_str.clone(),
    };

    let return_type = if ts_response_type == "void" {
        TsType::Ref("Promise<void>".into())
    } else if fetch.response.has_void_status {
        TsType::Ref(format!("Promise<{{ data: {} }} | void>", ts_response_type))
    } else {
        TsType::Ref(format!("Promise<{{ data: {} }}>", ts_response_type))
    };

    // Build function body
    let body = codegen_fetch_body(fetch, body_content_type, &ts_response_type);

    TsFunction {
        name: fetch.fn_name.clone(),
        type_params: vec![],
        params,
        return_type: Some(return_type),
        body,
        is_async: true,
        is_export: true,
        is_arrow: true,
    }
}

/// Generate the body of a fetch function.
fn codegen_fetch_body(
    fetch: &FetchIR,
    body_content_type: Option<BodyContentType>,
    ts_response_type: &str,
) -> Vec<TsStmt> {
    let mut stmts = Vec::new();

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
            // Create URLSearchParams
            stmts.push(TsStmt::VarDecl {
                kind: VarKind::Const,
                name: "searchParams".into(),
                ty: None,
                init: TsExpr::New {
                    callee: Box::new(TsExpr::Ident("URLSearchParams".into())),
                    args: vec![],
                },
            });

            // Add query params
            for qp in &fetch.url.query_params {
                let access = format_param_access("params", &qp.name, qp.required);

                if qp.ty.is_array() {
                    // Array params: forEach with append
                    stmts.push(TsStmt::Raw(format!(
                        "if ({} != null) {}.forEach((v) => searchParams.append(\"{}\", String(v)));",
                        access, access, qp.original_name
                    )));
                } else {
                    // Single params: set
                    stmts.push(TsStmt::Raw(format!(
                        "if ({} != null) searchParams.set(\"{}\", String({}));",
                        access, qp.original_name, access
                    )));
                }
            }

            // Build URL with query string
            stmts.push(TsStmt::VarDecl {
                kind: VarKind::Const,
                name: "queryString".into(),
                ty: None,
                init: TsExpr::Call {
                    callee: Box::new(TsExpr::Member {
                        object: Box::new(TsExpr::Ident("searchParams".into())),
                        prop: "toString".into(),
                    }),
                    args: vec![],
                },
            });

            stmts.push(TsStmt::VarDecl {
                kind: VarKind::Const,
                name: "url".into(),
                ty: None,
                init: TsExpr::Ternary {
                    cond: Box::new(TsExpr::Ident("queryString".into())),
                    then_expr: Box::new(TsExpr::Raw(format!(
                        "`{}?${{queryString}}`",
                        path_template
                    ))),
                    else_expr: Box::new(TsExpr::Raw(format!("`{}`", path_template))),
                },
            });

            // Fetch call with url variable
            stmts.push(codegen_fetch_call(
                "url",
                true,
                fetch,
                body_content_type,
            ));
        } else {
            // Just path params, use template literal directly
            stmts.push(codegen_fetch_call(
                &format!("`{}`", path_template),
                false,
                fetch,
                body_content_type,
            ));
        }
    } else {
        // No params at all - static URL
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

        stmts.push(codegen_fetch_call(
            &format!("\"{}\"", path),
            false,
            fetch,
            body_content_type,
        ));
    }

    // Error handling
    stmts.push(TsStmt::Raw(
        r#"if (!res.ok) {
  const body = await res.text();
  let parsed: unknown;
  try { parsed = JSON.parse(body); } catch { parsed = body; }
  throw new ApiError(res.status, res.statusText, parsed);
}"#
        .into(),
    ));

    // Return statement based on response type
    if ts_response_type == "void" {
        stmts.push(TsStmt::Return(None));
    } else if fetch.response.has_void_status {
        stmts.push(TsStmt::Raw("if (res.status === 204) return;".into()));
        let method = response_method_for_content_type(fetch.response.content_type);
        stmts.push(TsStmt::Raw(format!(
            "return {{ data: await res.{}() }};",
            method
        )));
    } else {
        let method = response_method_for_content_type(fetch.response.content_type);
        stmts.push(TsStmt::Raw(format!(
            "return {{ data: await res.{}() }};",
            method
        )));
    }

    stmts
}

/// Generate the fetch() call statement.
fn codegen_fetch_call(
    url_expr: &str,
    is_variable: bool,
    fetch: &FetchIR,
    body_content_type: Option<BodyContentType>,
) -> TsStmt {
    let mut fetch_options = format!("...options, method: \"{}\"", fetch.method.as_str());

    let has_header_params = !fetch.header_params.is_empty();
    let has_body = fetch.body.is_some();

    if has_body || has_header_params {
        fetch_options.push_str(", headers: { ");

        // Add content-type header
        if let Some(content_type) = body_content_type {
            match content_type {
                BodyContentType::Json => {
                    fetch_options.push_str("\"Content-Type\": \"application/json\", ");
                }
                BodyContentType::UrlEncoded => {
                    fetch_options.push_str("\"Content-Type\": \"application/x-www-form-urlencoded\", ");
                }
                BodyContentType::FormData => {
                    // Don't set Content-Type for FormData - browser sets it with boundary
                }
            }
        }

        // Add header params
        for hp in &fetch.header_params {
            if hp.required {
                let access = format_param_access("params", &hp.name, true);
                fetch_options.push_str(&format!("\"{}\": {}, ", hp.original_name, access));
            } else {
                let access = format_param_access("params", &hp.name, false);
                let direct_access = format_param_access("params", &hp.name, true);
                fetch_options.push_str(&format!(
                    "...({} != null && {{ \"{}\": {} }}), ",
                    access, hp.original_name, direct_access
                ));
            }
        }

        fetch_options.push_str("...options?.headers }");

        // Add body
        if has_body {
            match body_content_type {
                Some(BodyContentType::Json) => {
                    fetch_options.push_str(", body: JSON.stringify(data)");
                }
                Some(BodyContentType::UrlEncoded) => {
                    fetch_options
                        .push_str(", body: new URLSearchParams(data as Record<string, string>)");
                }
                Some(BodyContentType::FormData) => {
                    fetch_options.push_str(", body: data");
                }
                None => {}
            }
        }
    }

    // Generate the full fetch statement
    let url_part = if is_variable {
        url_expr.to_string()
    } else {
        url_expr.to_string()
    };

    TsStmt::VarDecl {
        kind: VarKind::Const,
        name: "res".into(),
        ty: None,
        init: TsExpr::Raw(format!(
            "await fetch({}, {{ {} }})",
            url_part, fetch_options
        )),
    }
}

/// Build path template string for URL construction.
fn build_path_template_string(template: &[UrlPart]) -> String {
    template
        .iter()
        .map(|p| match p {
            UrlPart::Static(s) => s.clone(),
            UrlPart::Param(name) => {
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

/// Get response method based on content type.
fn response_method_for_content_type(content_type: ResponseContentType) -> &'static str {
    match content_type {
        ResponseContentType::Json => "json",
        ResponseContentType::Text => "text",
        ResponseContentType::Blob => "blob",
        ResponseContentType::Unknown => "json",
    }
}

/// Generate a query key function.
fn codegen_query_key_function(qk: &QueryKeyIR) -> TsFunction {
    let base_key_lit = TsExpr::Literal(TsLiteral::String(qk.base_key.clone()));

    let (params, body_expr) = if let Some(params_type) = &qk.params_type {
        let params = vec![TsParam {
            name: "params".into(),
            ty: Some(params_type.to_ts_type()),
            optional: true,
        }];
        let body_expr = TsExpr::Cast {
            expr: Box::new(TsExpr::Array(vec![
                base_key_lit,
                TsExpr::Ident("params".into()),
            ])),
            ty: TsType::Ref("const".into()),
        };
        (params, body_expr)
    } else {
        let body_expr = TsExpr::Cast {
            expr: Box::new(TsExpr::Array(vec![base_key_lit])),
            ty: TsType::Ref("const".into()),
        };
        (vec![], body_expr)
    };

    // For query key, we output a simple arrow function that returns an array
    // export const listItemsKey = (params?: ParamsType) => ["/items", params] as const;
    TsFunction {
        name: qk.fn_name.clone(),
        type_params: vec![],
        params,
        return_type: None,
        body: vec![TsStmt::Raw(format!(
            "return {};",
            body_expr.emit()
        ))],
        is_async: false,
        is_export: true,
        is_arrow: true,
    }
}

/// Generate a React Query hook.
fn codegen_hook(hook: &HookIR) -> TsFunction {
    let response_str = hook.response_type.emit();
    let wrapped_type = if response_str == "void" {
        "void".to_string()
    } else {
        format!("{{ data: {} }}", response_str)
    };

    match hook.kind {
        HookKind::Query | HookKind::SuspenseQuery => {
            codegen_query_hook(hook, &wrapped_type)
        }
        HookKind::Mutation => codegen_mutation_hook(hook, &wrapped_type),
    }
}

/// Generate a query hook (useQuery or useSuspenseQuery).
fn codegen_query_hook(hook: &HookIR, wrapped_type: &str) -> TsFunction {
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

    let (options_param_type, body) = if let Some(vars) = &hook.vars_type {
        let vars_str = vars.emit();
        let options_type_str = format!(
            "{{ params?: {}; query?: Omit<{}<{}, ApiError, TData>, \"queryKey\" | \"queryFn\"> }}",
            vars_str, options_type, wrapped_type
        );
        let body = format!(
            "return {}({{ queryKey: {}(options?.params), queryFn: () => {}(options?.params), ...options?.query }});",
            hook_fn, key_fn, hook.fetch_fn
        );
        (options_type_str, body)
    } else {
        let options_type_str = format!(
            "{{ query?: Omit<{}<{}, ApiError, TData>, \"queryKey\" | \"queryFn\"> }}",
            options_type, wrapped_type
        );
        let body = format!(
            "return {}({{ queryKey: {}(), queryFn: () => {}(), ...options?.query }});",
            hook_fn, key_fn, hook.fetch_fn
        );
        (options_type_str, body)
    };

    TsFunction {
        name: hook.name.clone(),
        type_params: vec![format!("TData = {}", wrapped_type)],
        params: vec![TsParam {
            name: "options".into(),
            ty: Some(TsType::Ref(options_param_type)),
            optional: true,
        }],
        return_type: None,
        body: vec![TsStmt::Raw(body)],
        is_async: false,
        is_export: true,
        is_arrow: false,
    }
}

/// Generate a mutation hook.
fn codegen_mutation_hook(hook: &HookIR, wrapped_type: &str) -> TsFunction {
    let vars_str = hook
        .vars_type
        .as_ref()
        .map(|v| v.emit())
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

    let options_type_str = format!(
        "{{ mutation?: UseMutationOptions<{}, ApiError, {}> }}",
        wrapped_type, vars_str
    );

    let body = format!(
        "return useMutation({{ mutationFn: {}, ...options?.mutation }});",
        mutation_fn
    );

    TsFunction {
        name: hook.name.clone(),
        type_params: vec![],
        params: vec![TsParam {
            name: "options".into(),
            ty: Some(TsType::Ref(options_type_str)),
            optional: true,
        }],
        return_type: None,
        body: vec![TsStmt::Raw(body)],
        is_async: false,
        is_export: true,
        is_arrow: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codegen_imports_queries_only() {
        let imports = codegen_imports(true, false);
        assert_eq!(imports.len(), 2); // runtime + types

        let runtime = &imports[0];
        assert!(!runtime.type_only);
        assert!(runtime.items.iter().any(|i| i.name == "useQuery"));
        assert!(runtime.items.iter().any(|i| i.name == "useSuspenseQuery"));

        let types = &imports[1];
        assert!(types.type_only);
        assert!(types.items.iter().any(|i| i.name == "UseQueryOptions"));
    }

    #[test]
    fn test_codegen_imports_mutations_only() {
        let imports = codegen_imports(false, true);
        assert_eq!(imports.len(), 2);

        let runtime = &imports[0];
        assert!(runtime.items.iter().any(|i| i.name == "useMutation"));
    }

    #[test]
    fn test_codegen_query_key_no_params() {
        let qk = QueryKeyIR {
            fn_name: "listItemsKey".into(),
            base_key: "/items".into(),
            params_type: None,
        };
        let func = codegen_query_key_function(&qk);
        assert_eq!(func.name, "listItemsKey");
        assert!(func.is_export);
        assert!(func.is_arrow);
        assert!(func.params.is_empty());
    }

    #[test]
    fn test_codegen_query_key_with_params() {
        let qk = QueryKeyIR {
            fn_name: "getItemKey".into(),
            base_key: "/items/{id}".into(),
            params_type: Some(TypeRef::Named("GetItemParams".into())),
        };
        let func = codegen_query_key_function(&qk);
        assert_eq!(func.name, "getItemKey");
        assert_eq!(func.params.len(), 1);
        assert!(func.params[0].optional);
    }
}
