//! Code generation from Domain IR to SWC AST.
//!
//! This module transforms the high-level API IR (operations, hooks, params)
//! into `swc_ecma_ast::Module` nodes, which can then be emitted to TypeScript
//! strings via SWC's codegen.

use swc_common::DUMMY_SP;
use swc_ecma_ast::*;

use super::api::{
    ApiIR, BodyContentType, FetchArgIR, FetchIR, HookIR, HookKind, OperationIR, ParamsIR,
    QueryKeyIR, ResponseContentType, UrlPart,
};
use super::builders::*;
use super::types::{TsType as IrTsType, TypeRef};
use super::utils::{escape_js_string, needs_bracket_notation};

/// Generate a complete SWC Module from API IR.
pub fn codegen_module(api: &ApiIR) -> Module {
    let mut body = Vec::new();

    // Generate imports
    if api.has_queries || api.has_mutations {
        body.extend(codegen_imports(api.has_queries, api.has_mutations));
        // Generate ApiError class
        body.push(codegen_api_error_class());
    }

    // Add component schema types
    for td in &api.types {
        body.extend(ir_typedef_to_module_items(td));
    }

    // Generate operations
    for op in &api.operations {
        body.extend(codegen_operation(op));
    }

    Module {
        span: DUMMY_SP,
        body,
        shebang: None,
    }
}

/// Generate import statements.
fn codegen_imports(has_queries: bool, has_mutations: bool) -> Vec<ModuleItem> {
    let mut imports = Vec::new();

    let mut runtime_items: Vec<(&str, Option<&str>)> = Vec::new();
    let mut type_items: Vec<(&str, Option<&str>)> = Vec::new();

    if has_queries {
        runtime_items.push(("useQuery", None));
        runtime_items.push(("useSuspenseQuery", None));
        type_items.push(("UseQueryOptions", None));
        type_items.push(("UseSuspenseQueryOptions", None));
    }

    if has_mutations {
        runtime_items.push(("useMutation", None));
        type_items.push(("UseMutationOptions", None));
    }

    if !runtime_items.is_empty() {
        imports.push(import_named(runtime_items, "@tanstack/react-query", false));
    }

    if !type_items.is_empty() {
        imports.push(import_named(type_items, "@tanstack/react-query", true));
    }

    imports
}

/// Generate the ApiError class as a proper SWC ClassDecl.
fn codegen_api_error_class() -> ModuleItem {
    let status_prop = class_prop("status", ts_kw!(number));
    let status_text_prop = class_prop("statusText", ts_kw!(string));
    let body_prop = class_prop("body", ts_kw!(unknown));

    // constructor(status: number, statusText: string, body: unknown) { ... }
    let ctor = constructor(
        vec![
            constructor_param("status", Some(ts_kw!(number))),
            constructor_param("statusText", Some(ts_kw!(string))),
            constructor_param("body", Some(ts_kw!(unknown))),
        ],
        block(vec![
            // super(`HTTP ${status}: ${statusText}`)
            expr_stmt(Expr::Call(CallExpr {
                span: DUMMY_SP,
                ctxt: swc_common::SyntaxContext::empty(),
                callee: Callee::Super(Super { span: DUMMY_SP }),
                args: vec![arg(tpl(
                    vec!["HTTP ", ": ", ""],
                    vec![ident_expr("status"), ident_expr("statusText")],
                ))],
                type_args: None,
            })),
            // this.name = "ApiError"
            expr_stmt(assign_expr(
                member(Expr::This(ThisExpr { span: DUMMY_SP }), "name"),
                str_lit("ApiError"),
            )),
            // this.status = status
            expr_stmt(assign_expr(
                member(Expr::This(ThisExpr { span: DUMMY_SP }), "status"),
                ident_expr("status"),
            )),
            // this.statusText = statusText
            expr_stmt(assign_expr(
                member(Expr::This(ThisExpr { span: DUMMY_SP }), "statusText"),
                ident_expr("statusText"),
            )),
            // this.body = body
            expr_stmt(assign_expr(
                member(Expr::This(ThisExpr { span: DUMMY_SP }), "body"),
                ident_expr("body"),
            )),
        ]),
    );

    export_class(
        "ApiError",
        Some("Error"),
        vec![status_prop, status_text_prop, body_prop, ctor],
    )
}

/// Generate code for a single operation.
fn codegen_operation(op: &OperationIR) -> Vec<ModuleItem> {
    let mut items = Vec::new();

    // Generate params interface
    if let Some(params) = &op.params {
        items.push(codegen_params_type(params));
    }

    // Generate fetch function
    items.push(codegen_fetch_function(&op.fetch));

    // Generate query key function
    if let Some(qk) = &op.query_key {
        items.push(codegen_query_key_function(qk));
    }

    // Generate hooks
    for hook in &op.hooks {
        items.push(codegen_hook(hook));
    }

    items
}

/// Generate a params interface type.
fn codegen_params_type(params: &ParamsIR) -> ModuleItem {
    let properties = params
        .fields
        .iter()
        .map(|field| ts_property_sig(&field.name, ir_typeref_to_swc(&field.ty), !field.required))
        .collect();

    export_interface(&params.type_name, properties)
}

/// Generate a fetch function.
fn codegen_fetch_function(fetch: &FetchIR) -> ModuleItem {
    let mut params = Vec::new();
    let mut body_content_type = None;

    // Build parameters
    for a in &fetch.args {
        match a {
            FetchArgIR::Params { ty, optional } => {
                let swc_ty = ir_typeref_to_swc(ty);
                params.push(pat_ident("params", Some(swc_ty), *optional));
            }
            FetchArgIR::Body { ty, content_type } => {
                body_content_type = Some(*content_type);
                let ty_type = match content_type {
                    BodyContentType::FormData => ts_type_ref("FormData"),
                    BodyContentType::UrlEncoded | BodyContentType::Json => ir_typeref_to_swc(ty),
                };
                params.push(pat_ident("data", Some(ty_type), false));
            }
            FetchArgIR::Options => {
                params.push(pat_ident("options", Some(ts_type_ref("RequestInit")), true));
            }
        }
    }

    // Build return type
    let response_swc_type = resolve_content_type(fetch.response.content_type, &fetch.response.ty);
    let is_void_response = is_void_type(&response_swc_type);

    let return_type = if is_void_response {
        promise_type(ts_kw!(void))
    } else if fetch.response.has_void_status {
        promise_type(ts_union(vec![
            data_wrapper_type(response_swc_type),
            ts_kw!(void),
        ]))
    } else {
        promise_type(data_wrapper_type(response_swc_type))
    };

    // Build function body
    let body_stmts = codegen_fetch_body(fetch, body_content_type, is_void_response);

    export_const_arrow(
        &fetch.fn_name,
        params,
        Some(return_type),
        block(body_stmts),
        true,
    )
}

/// Generate the body of a fetch function.
fn codegen_fetch_body(
    fetch: &FetchIR,
    body_content_type: Option<BodyContentType>,
    is_void_response: bool,
) -> Vec<Stmt> {
    let mut stmts = Vec::new();

    // URL building
    let has_path_params = fetch
        .url
        .template
        .iter()
        .any(|p| matches!(p, UrlPart::Param(_)));
    let has_query_params = !fetch.url.query_params.is_empty();

    if has_path_params || has_query_params {
        if has_query_params {
            // const searchParams = new URLSearchParams()
            stmts.push(const_decl(
                "searchParams",
                new_expr(ident_expr("URLSearchParams"), vec![]),
            ));

            // Add query params
            for qp in &fetch.url.query_params {
                let access_expr = build_param_access_expr("params", &qp.name, qp.required);

                if qp.ty.is_array() {
                    // if (access != null) access.forEach((v) => searchParams.append("name", String(v)));
                    let check_expr = not_null_check(access_expr.clone());
                    let foreach_call = call(
                        member(access_expr, "forEach"),
                        vec![arrow_fn_expr(
                            vec![pat_ident("v", None, false)],
                            call(
                                member(ident_expr("searchParams"), "append"),
                                vec![
                                    str_lit(&qp.original_name),
                                    call(ident_expr("String"), vec![ident_expr("v")]),
                                ],
                            ),
                        )],
                    );
                    stmts.push(if_stmt(check_expr, expr_stmt(foreach_call), None));
                } else {
                    // if (access != null) searchParams.set("name", String(access));
                    let check_expr = not_null_check(access_expr.clone());
                    let set_call = call(
                        member(ident_expr("searchParams"), "set"),
                        vec![
                            str_lit(&qp.original_name),
                            call(ident_expr("String"), vec![access_expr]),
                        ],
                    );
                    stmts.push(if_stmt(check_expr, expr_stmt(set_call), None));
                }
            }

            // const queryString = searchParams.toString()
            stmts.push(const_decl(
                "queryString",
                call(member(ident_expr("searchParams"), "toString"), vec![]),
            ));

            // const url = queryString ? `path?${queryString}` : `path`
            let path_template = build_path_template(&fetch.url.template);
            let (path_quasis_q, path_exprs_q) =
                build_tpl_parts_with_suffix(&fetch.url.template, Some("queryString"));
            let (path_quasis, path_exprs) = build_tpl_parts_with_suffix(&fetch.url.template, None);

            let url_with_qs = tpl(
                path_quasis_q.iter().map(|s| s.as_str()).collect(),
                path_exprs_q,
            );
            let url_without_qs = if path_exprs.is_empty() {
                str_lit(&path_template)
            } else {
                tpl(path_quasis.iter().map(|s| s.as_str()).collect(), path_exprs)
            };

            stmts.push(const_decl(
                "url",
                cond_expr(ident_expr("queryString"), url_with_qs, url_without_qs),
            ));

            // Fetch call with url variable
            stmts.push(codegen_fetch_call_stmt(
                ident_expr("url"),
                fetch,
                body_content_type,
            ));
        } else {
            // Just path params, use template literal directly
            let (quasis, exprs) = build_tpl_parts_with_suffix(&fetch.url.template, None);
            let url_expr = tpl(quasis.iter().map(|s| s.as_str()).collect(), exprs);
            stmts.push(codegen_fetch_call_stmt(url_expr, fetch, body_content_type));
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
        stmts.push(codegen_fetch_call_stmt(
            str_lit(&path),
            fetch,
            body_content_type,
        ));
    }

    // Error handling: if (!res.ok) { ... }
    stmts.push(codegen_error_handling());

    // Return statement based on response type
    if is_void_response {
        stmts.push(return_stmt(None));
    } else if fetch.response.has_void_status {
        // if (res.status === 204) return;
        stmts.push(if_stmt(
            bin_expr(
                member(ident_expr("res"), "status"),
                BinaryOp::EqEqEq,
                num_lit(204.0),
            ),
            return_stmt(None),
            None,
        ));
        let data_expr = response_data_expr(fetch.response.content_type);
        stmts.push(return_stmt(Some(obj_lit(vec![kv_prop("data", data_expr)]))));
    } else {
        let data_expr = response_data_expr(fetch.response.content_type);
        stmts.push(return_stmt(Some(obj_lit(vec![kv_prop("data", data_expr)]))));
    }

    stmts
}

/// Generate the `const res = await fetch(url, { ... })` statement.
fn codegen_fetch_call_stmt(
    url_expr: Expr,
    fetch: &FetchIR,
    body_content_type: Option<BodyContentType>,
) -> Stmt {
    let has_header_params = !fetch.header_params.is_empty();
    let has_body = fetch.body.is_some();

    let mut fetch_props: Vec<PropOrSpread> = vec![
        spread_prop(ident_expr("options")),
        kv_prop("method", str_lit(fetch.method.as_str())),
    ];

    if has_body || has_header_params {
        let mut header_props: Vec<PropOrSpread> = Vec::new();

        // Add content-type header
        if let Some(content_type) = body_content_type {
            match content_type {
                BodyContentType::Json => {
                    header_props.push(kv_prop_str("Content-Type", str_lit("application/json")));
                }
                BodyContentType::UrlEncoded => {
                    header_props.push(kv_prop_str(
                        "Content-Type",
                        str_lit("application/x-www-form-urlencoded"),
                    ));
                }
                BodyContentType::FormData => {
                    // Don't set Content-Type for FormData - browser sets it with boundary
                }
            }
        }

        // Add header params
        for hp in &fetch.header_params {
            if hp.required {
                let access = build_param_access_expr("params", &hp.name, true);
                header_props.push(kv_prop_str(&hp.original_name, access));
            } else {
                // ...( access != null && { "name": direct_access } )
                let access = build_param_access_expr("params", &hp.name, false);
                let direct_access = build_param_access_expr("params", &hp.name, true);
                let conditional = bin_expr(
                    not_null_check(access),
                    BinaryOp::LogicalAnd,
                    obj_lit(vec![kv_prop_str(&hp.original_name, direct_access)]),
                );
                header_props.push(spread_prop(paren(conditional)));
            }
        }

        // ...options?.headers
        header_props.push(spread_prop(opt_chain_member(
            ident_expr("options"),
            "headers",
        )));

        fetch_props.push(kv_prop("headers", obj_lit(header_props)));

        // Add body
        if has_body {
            match body_content_type {
                Some(BodyContentType::Json) => {
                    fetch_props.push(kv_prop(
                        "body",
                        call(
                            member(ident_expr("JSON"), "stringify"),
                            vec![ident_expr("data")],
                        ),
                    ));
                }
                Some(BodyContentType::UrlEncoded) => {
                    fetch_props.push(kv_prop(
                        "body",
                        new_expr(
                            ident_expr("URLSearchParams"),
                            vec![ts_as_expr(
                                ident_expr("data"),
                                ts_type_ref_with_params(
                                    "Record",
                                    vec![ts_kw!(string), ts_kw!(string)],
                                ),
                            )],
                        ),
                    ));
                }
                Some(BodyContentType::FormData) => {
                    fetch_props.push(kv_prop("body", ident_expr("data")));
                }
                None => {}
            }
        }
    }

    let fetch_call = await_expr(call(
        ident_expr("fetch"),
        vec![url_expr, obj_lit(fetch_props)],
    ));

    const_decl("res", fetch_call)
}

/// Generate error handling block:
/// ```ts
/// if (!res.ok) {
///   const body = await res.text();
///   let parsed: unknown;
///   try { parsed = JSON.parse(body); } catch { parsed = body; }
///   throw new ApiError(res.status, res.statusText, parsed);
/// }
/// ```
fn codegen_error_handling() -> Stmt {
    let body_decl = const_decl(
        "body",
        await_expr(call(member(ident_expr("res"), "text"), vec![])),
    );

    let parsed_decl = Stmt::Decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: swc_common::SyntaxContext::empty(),
        kind: VarDeclKind::Let,
        declare: false,
        decls: vec![VarDeclarator {
            span: DUMMY_SP,
            name: Pat::Ident(binding_ident("parsed", Some(ts_kw!(unknown)), false)),
            init: None,
            definite: false,
        }],
    })));

    let try_block = block(vec![expr_stmt(Expr::Assign(AssignExpr {
        span: DUMMY_SP,
        op: AssignOp::Assign,
        left: AssignTarget::Simple(SimpleAssignTarget::Ident(binding_ident(
            "parsed", None, false,
        ))),
        right: Box::new(call(
            member(ident_expr("JSON"), "parse"),
            vec![ident_expr("body")],
        )),
    }))]);

    let catch_block = block(vec![expr_stmt(Expr::Assign(AssignExpr {
        span: DUMMY_SP,
        op: AssignOp::Assign,
        left: AssignTarget::Simple(SimpleAssignTarget::Ident(binding_ident(
            "parsed", None, false,
        ))),
        right: Box::new(ident_expr("body")),
    }))]);

    let try_catch = try_stmt(try_block, Some(catch_block));

    let throw = throw_stmt(new_expr(
        ident_expr("ApiError"),
        vec![
            member(ident_expr("res"), "status"),
            member(ident_expr("res"), "statusText"),
            ident_expr("parsed"),
        ],
    ));

    if_stmt(
        unary_not(member(ident_expr("res"), "ok")),
        block_stmt(vec![body_decl, parsed_decl, try_catch, throw]),
        None,
    )
}

/// Get response data expression based on content type.
fn response_data_expr(content_type: ResponseContentType) -> Expr {
    match content_type {
        ResponseContentType::Json => await_expr(call(member(ident_expr("res"), "json"), vec![])),
        ResponseContentType::Text => await_expr(call(member(ident_expr("res"), "text"), vec![])),
        ResponseContentType::Blob => await_expr(call(member(ident_expr("res"), "blob"), vec![])),
        ResponseContentType::Unknown => ident_expr("res"),
    }
}

/// Resolve the SWC type for a response based on content type.
fn resolve_content_type(
    content_type: ResponseContentType,
    ty: &TypeRef,
) -> Box<swc_ecma_ast::TsType> {
    match content_type {
        ResponseContentType::Text => ts_kw!(string),
        ResponseContentType::Blob => ts_type_ref("Blob"),
        ResponseContentType::Unknown => ts_type_ref("Response"),
        ResponseContentType::Json => ir_typeref_to_swc(ty),
    }
}

/// Check if an SWC type is the `void` keyword.
fn is_void_type(ty: &swc_ecma_ast::TsType) -> bool {
    matches!(
        ty,
        swc_ecma_ast::TsType::TsKeywordType(swc_ecma_ast::TsKeywordType {
            kind: TsKeywordTypeKind::TsVoidKeyword,
            ..
        })
    )
}

/// Build a plain path template string (for fallback/display).
fn build_path_template(template: &[UrlPart]) -> String {
    template
        .iter()
        .map(|p| match p {
            UrlPart::Static(s) => s.clone(),
            UrlPart::Param(name) => {
                if needs_bracket_notation(name) {
                    format!("${{params[\"{}\"]}}", escape_js_string(name))
                } else {
                    format!("${{params.{name}}}")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Build template literal quasis and expressions for a URL template.
/// If `suffix_var` is provided, appends `?${suffix_var}` to the template.
fn build_tpl_parts_with_suffix(
    template: &[UrlPart],
    suffix_var: Option<&str>,
) -> (Vec<String>, Vec<Expr>) {
    let mut quasis = Vec::new();
    let mut exprs = Vec::new();
    let mut current_static = String::new();

    for part in template {
        match part {
            UrlPart::Static(s) => {
                current_static.push_str(s);
            }
            UrlPart::Param(name) => {
                quasis.push(current_static.clone());
                current_static.clear();
                exprs.push(build_param_access_expr("params", name, true));
            }
        }
    }

    if let Some(var) = suffix_var {
        current_static.push('?');
        quasis.push(current_static);
        exprs.push(ident_expr(var));
        quasis.push(String::new());
    } else {
        quasis.push(current_static);
    }

    (quasis, exprs)
}

/// Build an expression that accesses a parameter, handling bracket notation and optional chaining.
fn build_param_access_expr(obj: &str, prop: &str, required: bool) -> Expr {
    if needs_bracket_notation(prop) {
        let key = str_lit(&escape_js_string(prop));
        if required {
            computed_member(ident_expr(obj), key)
        } else {
            opt_chain_computed(ident_expr(obj), key)
        }
    } else if required {
        member(ident_expr(obj), prop)
    } else {
        opt_chain_member(ident_expr(obj), prop)
    }
}

/// Generate a query key function.
fn codegen_query_key_function(qk: &QueryKeyIR) -> ModuleItem {
    let base_key = str_lit(&qk.base_key);

    let (params, body_expr) = if let Some(params_type) = &qk.params_type {
        let params = vec![pat_ident(
            "params",
            Some(ir_typeref_to_swc(params_type)),
            true,
        )];
        let body_expr = as_const(array_lit(vec![base_key, ident_expr("params")]));
        (params, body_expr)
    } else {
        let body_expr = as_const(array_lit(vec![base_key]));
        (vec![], body_expr)
    };

    export_const_arrow(
        &qk.fn_name,
        params,
        None,
        block(vec![return_stmt(Some(body_expr))]),
        false,
    )
}

/// Generate a React Query hook.
fn codegen_hook(hook: &HookIR) -> ModuleItem {
    let response_swc_type = resolve_content_type(hook.response_content_type, &hook.response_type);
    let is_void = is_void_type(&response_swc_type);

    let wrapped_type: Box<swc_ecma_ast::TsType> = if is_void {
        ts_kw!(void)
    } else if hook.response_has_void_status {
        ts_union(vec![data_wrapper_type(response_swc_type), ts_kw!(void)])
    } else {
        data_wrapper_type(response_swc_type)
    };

    match hook.kind {
        HookKind::Query | HookKind::SuspenseQuery => codegen_query_hook(hook, wrapped_type),
        HookKind::Mutation => codegen_mutation_hook(hook, wrapped_type),
    }
}

/// Build `Omit<OptionsType<Wrapped, ApiError, TData>, "queryKey" | "queryFn">`.
fn omit_query_opts(
    options_type: &str,
    wrapped: &swc_ecma_ast::TsType,
) -> Box<swc_ecma_ast::TsType> {
    let opts = ts_type_ref_with_params(
        options_type,
        vec![
            Box::new(wrapped.clone()),
            ts_type_ref("ApiError"),
            ts_type_ref("TData"),
        ],
    );
    ts_omit(
        opts,
        ts_union(vec![ts_lit_str("queryKey"), ts_lit_str("queryFn")]),
    )
}

/// Generate a query hook (useQuery or useSuspenseQuery).
#[allow(clippy::expect_used)]
fn codegen_query_hook(hook: &HookIR, wrapped_type: Box<swc_ecma_ast::TsType>) -> ModuleItem {
    let key_fn = hook
        .query_key_fn
        .as_ref()
        .expect("query_key_fn must be set for query hooks");
    let hook_fn = if hook.kind == HookKind::Query {
        "useQuery"
    } else {
        "useSuspenseQuery"
    };
    let options_type_name = if hook.kind == HookKind::Query {
        "UseQueryOptions"
    } else {
        "UseSuspenseQueryOptions"
    };

    let (options_param_type, body_stmt, options_optional) = if let Some(vars) = &hook.vars_type {
        let vars_swc = ir_typeref_to_swc(vars);
        let params_prop = ts_property_sig("params", vars_swc, !hook.params_required);
        let query_prop = ts_property_sig(
            "query",
            omit_query_opts(options_type_name, &wrapped_type),
            true,
        );
        let opts_type = ts_object_type(vec![params_prop, query_prop]);

        let param_access_expr: Expr = if hook.params_required {
            member(ident_expr("options"), "params")
        } else {
            opt_chain_member(ident_expr("options"), "params")
        };

        let hook_call = call(
            ident_expr(hook_fn),
            vec![obj_lit(vec![
                kv_prop(
                    "queryKey",
                    call(ident_expr(key_fn), vec![param_access_expr.clone()]),
                ),
                kv_prop(
                    "queryFn",
                    arrow_fn_expr(
                        vec![],
                        call(ident_expr(&hook.fetch_fn), vec![param_access_expr]),
                    ),
                ),
                spread_prop(opt_chain_member(ident_expr("options"), "query")),
            ])],
        );

        let body = return_stmt(Some(hook_call));
        (opts_type, body, !hook.params_required)
    } else {
        let query_prop = ts_property_sig(
            "query",
            omit_query_opts(options_type_name, &wrapped_type),
            true,
        );
        let opts_type = ts_object_type(vec![query_prop]);

        let hook_call = call(
            ident_expr(hook_fn),
            vec![obj_lit(vec![
                kv_prop("queryKey", call(ident_expr(key_fn), vec![])),
                kv_prop(
                    "queryFn",
                    arrow_fn_expr(vec![], call(ident_expr(&hook.fetch_fn), vec![])),
                ),
                spread_prop(opt_chain_member(ident_expr("options"), "query")),
            ])],
        );

        let body = return_stmt(Some(hook_call));
        (opts_type, body, true)
    };

    let tdata_param = ts_type_param("TData", Some(wrapped_type));

    let options_param = param("options", Some(options_param_type), options_optional);

    export_function(
        &hook.name,
        Some(vec![tdata_param]),
        vec![options_param],
        None,
        block(vec![body_stmt]),
        false,
    )
}

/// Generate a mutation hook.
fn codegen_mutation_hook(hook: &HookIR, wrapped_type: Box<swc_ecma_ast::TsType>) -> ModuleItem {
    let vars_swc_type = hook
        .vars_type
        .as_ref()
        .map(ir_typeref_to_swc)
        .unwrap_or_else(|| ts_kw!(void));

    // Build mutation function expression
    let mutation_fn = if let Some(vars) = &hook.vars_type {
        match vars {
            TypeRef::Inline(t) => match &**t {
                IrTsType::Object(props) => {
                    let has_params = props.iter().any(|p| p.name == "params");
                    let has_data = props.iter().any(|p| p.name == "data");
                    match (has_params, has_data) {
                        (true, true) => {
                            if hook.body_before_params {
                                arrow_fn_expr(
                                    vec![pat_ident("vars", None, false)],
                                    call(
                                        ident_expr(&hook.fetch_fn),
                                        vec![
                                            member(ident_expr("vars"), "data"),
                                            member(ident_expr("vars"), "params"),
                                        ],
                                    ),
                                )
                            } else {
                                arrow_fn_expr(
                                    vec![pat_ident("vars", None, false)],
                                    call(
                                        ident_expr(&hook.fetch_fn),
                                        vec![
                                            member(ident_expr("vars"), "params"),
                                            member(ident_expr("vars"), "data"),
                                        ],
                                    ),
                                )
                            }
                        }
                        (true, false) => arrow_fn_expr(
                            vec![pat_ident("vars", None, false)],
                            call(
                                ident_expr(&hook.fetch_fn),
                                vec![member(ident_expr("vars"), "params")],
                            ),
                        ),
                        _ => arrow_fn_expr(
                            vec![pat_ident("data", None, false)],
                            call(ident_expr(&hook.fetch_fn), vec![ident_expr("data")]),
                        ),
                    }
                }
                _ => arrow_fn_expr(
                    vec![pat_ident("data", None, false)],
                    call(ident_expr(&hook.fetch_fn), vec![ident_expr("data")]),
                ),
            },
            TypeRef::Named(_) => arrow_fn_expr(
                vec![pat_ident("data", None, false)],
                call(ident_expr(&hook.fetch_fn), vec![ident_expr("data")]),
            ),
        }
    } else {
        arrow_fn_expr(vec![], call(ident_expr(&hook.fetch_fn), vec![]))
    };

    // { mutation?: UseMutationOptions<WrappedType, ApiError, VarsType> }
    let mutation_opts = ts_type_ref_with_params(
        "UseMutationOptions",
        vec![wrapped_type, ts_type_ref("ApiError"), vars_swc_type],
    );
    let options_type = ts_object_type(vec![ts_property_sig("mutation", mutation_opts, true)]);

    let mutation_call = call(
        ident_expr("useMutation"),
        vec![obj_lit(vec![
            kv_prop("mutationFn", mutation_fn),
            spread_prop(opt_chain_member(ident_expr("options"), "mutation")),
        ])],
    );

    let body = return_stmt(Some(mutation_call));

    export_function(
        &hook.name,
        None,
        vec![param("options", Some(options_type), true)],
        None,
        block(vec![body]),
        false,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::api::*;
    use super::*;

    #[test]
    fn test_codegen_imports_queries_only() {
        let items = codegen_imports(true, false);
        assert_eq!(items.len(), 2); // runtime + types

        // Verify runtime import
        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = &items[0] {
            assert!(!import.type_only);
            assert!(import.specifiers.iter().any(
                |s| matches!(s, ImportSpecifier::Named(n) if n.local.sym.as_ref() == "useQuery")
            ));
            assert!(import.specifiers.iter().any(|s| matches!(s, ImportSpecifier::Named(n) if n.local.sym.as_ref() == "useSuspenseQuery")));
        } else {
            panic!("Expected import declaration");
        }

        // Verify type import
        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = &items[1] {
            assert!(import.type_only);
            assert!(import.specifiers.iter().any(|s| matches!(s, ImportSpecifier::Named(n) if n.local.sym.as_ref() == "UseQueryOptions")));
        } else {
            panic!("Expected type import declaration");
        }
    }

    #[test]
    fn test_codegen_imports_mutations_only() {
        let items = codegen_imports(false, true);
        assert_eq!(items.len(), 2);

        if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = &items[0] {
            assert!(import.specifiers.iter().any(
                |s| matches!(s, ImportSpecifier::Named(n) if n.local.sym.as_ref() == "useMutation")
            ));
        } else {
            panic!("Expected import declaration");
        }
    }

    #[test]
    fn test_codegen_query_key_no_params() {
        let qk = QueryKeyIR {
            fn_name: "listItemsKey".into(),
            base_key: "/items".into(),
            params_type: None,
        };
        let item = codegen_query_key_function(&qk);

        // Should be an export const arrow
        if let ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
            decl: Decl::Var(var_decl),
            ..
        })) = &item
        {
            assert_eq!(var_decl.kind, VarDeclKind::Const);
            let declarator = &var_decl.decls[0];
            if let Pat::Ident(bi) = &declarator.name {
                assert_eq!(bi.id.sym.as_ref(), "listItemsKey");
            }
            // Check it's an arrow function with no params
            if let Some(init) = &declarator.init {
                if let Expr::Arrow(arrow) = &**init {
                    assert!(arrow.params.is_empty());
                } else {
                    panic!("Expected arrow expression");
                }
            }
        } else {
            panic!("Expected export var declaration");
        }
    }

    #[test]
    fn test_codegen_query_key_with_params() {
        let qk = QueryKeyIR {
            fn_name: "getItemKey".into(),
            base_key: "/items/{id}".into(),
            params_type: Some(TypeRef::Named("GetItemParams".into())),
        };
        let item = codegen_query_key_function(&qk);

        if let ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
            decl: Decl::Var(var_decl),
            ..
        })) = &item
        {
            let declarator = &var_decl.decls[0];
            if let Some(init) = &declarator.init {
                if let Expr::Arrow(arrow) = &**init {
                    assert_eq!(arrow.params.len(), 1);
                    if let Pat::Ident(bi) = &arrow.params[0] {
                        assert!(bi.id.optional);
                    }
                } else {
                    panic!("Expected arrow expression");
                }
            }
        } else {
            panic!("Expected export var declaration");
        }
    }
}
