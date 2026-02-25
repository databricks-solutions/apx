//! Ergonomic builder helpers for constructing SWC AST nodes.
//!
//! These helpers reduce the verbosity of building `swc_ecma_ast` types
//! when generating TypeScript code programmatically.
#![allow(clippy::vec_box)] // SWC's TsTypeParamInstantiation/TsUnionType require Vec<Box<TsType>>

use swc_atoms::Atom;
use swc_common::{DUMMY_SP, SyntaxContext};
// Reason: builder module uses most items from parent; explicit list would be unwieldy
#[allow(clippy::wildcard_imports)]
use swc_ecma_ast::*;

use super::types::{self as ir, TypeRef};

// =============================================================================
// Identifiers
// =============================================================================

pub fn ident(name: &str) -> Ident {
    Ident::new_no_ctxt(Atom::new(name), DUMMY_SP)
}

pub fn ident_name(name: &str) -> IdentName {
    IdentName {
        span: DUMMY_SP,
        sym: Atom::new(name),
    }
}

pub fn binding_ident(name: &str, ty: Option<Box<TsType>>, optional: bool) -> BindingIdent {
    BindingIdent {
        id: Ident {
            optional,
            ..ident(name)
        },
        type_ann: ty.map(ts_type_ann),
    }
}

// =============================================================================
// TypeScript Types
// =============================================================================

fn ts_keyword(kind: TsKeywordTypeKind) -> Box<TsType> {
    Box::new(TsType::TsKeywordType(TsKeywordType {
        span: DUMMY_SP,
        kind,
    }))
}

macro_rules! ts_kw {
    (string) => {
        $crate::openapi::ir::builders::ts_keyword_string()
    };
    (number) => {
        $crate::openapi::ir::builders::ts_keyword_number()
    };
    (boolean) => {
        $crate::openapi::ir::builders::ts_keyword_boolean()
    };
    (null) => {
        $crate::openapi::ir::builders::ts_keyword_null()
    };
    (void) => {
        $crate::openapi::ir::builders::ts_keyword_void()
    };
    (unknown) => {
        $crate::openapi::ir::builders::ts_keyword_unknown()
    };
}

// These are pub so the macro can reference them from other modules.
// Prefer using ts_kw!(...) directly.
pub fn ts_keyword_string() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsStringKeyword)
}
pub fn ts_keyword_number() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsNumberKeyword)
}
pub fn ts_keyword_boolean() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsBooleanKeyword)
}
pub fn ts_keyword_null() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsNullKeyword)
}
pub fn ts_keyword_void() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsVoidKeyword)
}
pub fn ts_keyword_unknown() -> Box<TsType> {
    ts_keyword(TsKeywordTypeKind::TsUnknownKeyword)
}

pub fn ts_type_ref(name: &str) -> Box<TsType> {
    Box::new(TsType::TsTypeRef(TsTypeRef {
        span: DUMMY_SP,
        type_name: TsEntityName::Ident(ident(name)),
        type_params: None,
    }))
}

pub fn ts_type_ref_with_params(name: &str, params: Vec<Box<TsType>>) -> Box<TsType> {
    Box::new(TsType::TsTypeRef(TsTypeRef {
        span: DUMMY_SP,
        type_name: TsEntityName::Ident(ident(name)),
        type_params: Some(Box::new(TsTypeParamInstantiation {
            span: DUMMY_SP,
            params,
        })),
    }))
}

pub fn ts_array(elem: Box<TsType>) -> Box<TsType> {
    Box::new(TsType::TsArrayType(TsArrayType {
        span: DUMMY_SP,
        elem_type: elem,
    }))
}

pub fn ts_union(types: Vec<Box<TsType>>) -> Box<TsType> {
    Box::new(TsType::TsUnionOrIntersectionType(
        TsUnionOrIntersectionType::TsUnionType(TsUnionType {
            span: DUMMY_SP,
            types,
        }),
    ))
}

pub fn ts_intersection(types: Vec<Box<TsType>>) -> Box<TsType> {
    Box::new(TsType::TsUnionOrIntersectionType(
        TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType {
            span: DUMMY_SP,
            types,
        }),
    ))
}

pub fn ts_lit_str(s: &str) -> Box<TsType> {
    Box::new(TsType::TsLitType(TsLitType {
        span: DUMMY_SP,
        lit: TsLit::Str(Str {
            span: DUMMY_SP,
            value: s.into(),
            raw: None,
        }),
    }))
}

pub fn ts_lit_num(n: f64) -> Box<TsType> {
    Box::new(TsType::TsLitType(TsLitType {
        span: DUMMY_SP,
        lit: TsLit::Number(Number {
            span: DUMMY_SP,
            value: n,
            raw: None,
        }),
    }))
}

pub fn ts_lit_bool(b: bool) -> Box<TsType> {
    Box::new(TsType::TsLitType(TsLitType {
        span: DUMMY_SP,
        lit: TsLit::Bool(Bool {
            span: DUMMY_SP,
            value: b,
        }),
    }))
}

pub fn ts_object_type(members: Vec<TsTypeElement>) -> Box<TsType> {
    Box::new(TsType::TsTypeLit(TsTypeLit {
        span: DUMMY_SP,
        members,
    }))
}

pub fn ts_property_sig(name: &str, ty: Box<TsType>, optional: bool) -> TsTypeElement {
    let key: Box<Expr> = if super::utils::needs_bracket_notation(name) {
        Box::new(Expr::Lit(Lit::Str(Str {
            span: DUMMY_SP,
            value: name.into(),
            raw: None,
        })))
    } else {
        Box::new(Expr::Ident(ident(name)))
    };
    TsTypeElement::TsPropertySignature(TsPropertySignature {
        span: DUMMY_SP,
        readonly: false,
        key,
        computed: false,
        optional,
        type_ann: Some(ts_type_ann(ty)),
    })
}

pub fn ts_type_ann(ty: Box<TsType>) -> Box<TsTypeAnn> {
    Box::new(TsTypeAnn {
        span: DUMMY_SP,
        type_ann: ty,
    })
}

pub fn ts_paren(ty: Box<TsType>) -> Box<TsType> {
    Box::new(TsType::TsParenthesizedType(TsParenthesizedType {
        span: DUMMY_SP,
        type_ann: ty,
    }))
}

/// `Promise<T>`
pub fn promise_type(inner: Box<TsType>) -> Box<TsType> {
    ts_type_ref_with_params("Promise", vec![inner])
}

/// `{ data: T }`
pub fn data_wrapper_type(ty: Box<TsType>) -> Box<TsType> {
    ts_object_type(vec![ts_property_sig("data", ty, false)])
}

/// `Omit<T, K>`
pub fn ts_omit(ty: Box<TsType>, keys: Box<TsType>) -> Box<TsType> {
    ts_type_ref_with_params("Omit", vec![ty, keys])
}

/// Create a `TsTypeParam` with an optional default type.
pub fn ts_type_param(name: &str, default: Option<Box<TsType>>) -> TsTypeParam {
    TsTypeParam {
        span: DUMMY_SP,
        name: ident(name),
        is_in: false,
        is_out: false,
        is_const: false,
        constraint: None,
        default,
    }
}

// =============================================================================
// Expressions
// =============================================================================

pub fn ident_expr(name: &str) -> Expr {
    Expr::Ident(ident(name))
}

pub fn str_lit(s: &str) -> Expr {
    Expr::Lit(Lit::Str(Str {
        span: DUMMY_SP,
        value: s.into(),
        raw: None,
    }))
}

pub fn num_lit(n: f64) -> Expr {
    Expr::Lit(Lit::Num(Number {
        span: DUMMY_SP,
        value: n,
        raw: None,
    }))
}

pub fn bool_lit(b: bool) -> Expr {
    Expr::Lit(Lit::Bool(Bool {
        span: DUMMY_SP,
        value: b,
    }))
}

pub fn null_lit() -> Expr {
    Expr::Lit(Lit::Null(Null { span: DUMMY_SP }))
}

pub fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    Expr::Call(CallExpr {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        callee: Callee::Expr(Box::new(callee)),
        args: args
            .into_iter()
            .map(|e| ExprOrSpread {
                spread: None,
                expr: Box::new(e),
            })
            .collect(),
        type_args: None,
    })
}

pub fn member(obj: Expr, prop: &str) -> Expr {
    Expr::Member(MemberExpr {
        span: DUMMY_SP,
        obj: Box::new(obj),
        prop: MemberProp::Ident(ident_name(prop)),
    })
}

pub fn computed_member(obj: Expr, prop: Expr) -> Expr {
    Expr::Member(MemberExpr {
        span: DUMMY_SP,
        obj: Box::new(obj),
        prop: MemberProp::Computed(ComputedPropName {
            span: DUMMY_SP,
            expr: Box::new(prop),
        }),
    })
}

pub fn opt_chain_member(obj: Expr, prop: &str) -> Expr {
    Expr::OptChain(OptChainExpr {
        span: DUMMY_SP,
        optional: true,
        base: Box::new(OptChainBase::Member(MemberExpr {
            span: DUMMY_SP,
            obj: Box::new(obj),
            prop: MemberProp::Ident(ident_name(prop)),
        })),
    })
}

pub fn opt_chain_computed(obj: Expr, prop: Expr) -> Expr {
    Expr::OptChain(OptChainExpr {
        span: DUMMY_SP,
        optional: true,
        base: Box::new(OptChainBase::Member(MemberExpr {
            span: DUMMY_SP,
            obj: Box::new(obj),
            prop: MemberProp::Computed(ComputedPropName {
                span: DUMMY_SP,
                expr: Box::new(prop),
            }),
        })),
    })
}

pub fn arrow_fn_expr(params: Vec<Pat>, body: Expr) -> Expr {
    Expr::Arrow(ArrowExpr {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        params,
        body: Box::new(BlockStmtOrExpr::Expr(Box::new(body))),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: None,
    })
}

pub fn await_expr(expr: Expr) -> Expr {
    Expr::Await(AwaitExpr {
        span: DUMMY_SP,
        arg: Box::new(expr),
    })
}

pub fn tpl(quasis: Vec<&str>, exprs: Vec<Expr>) -> Expr {
    let quasis = quasis
        .into_iter()
        .enumerate()
        .map(|(i, s)| TplElement {
            span: DUMMY_SP,
            tail: i == exprs.len(), // last element is tail
            cooked: None,
            raw: Atom::new(s),
        })
        .collect();
    Expr::Tpl(Tpl {
        span: DUMMY_SP,
        exprs: exprs.into_iter().map(Box::new).collect(),
        quasis,
    })
}

pub fn new_expr(callee: Expr, args: Vec<Expr>) -> Expr {
    Expr::New(NewExpr {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        callee: Box::new(callee),
        args: Some(
            args.into_iter()
                .map(|e| ExprOrSpread {
                    spread: None,
                    expr: Box::new(e),
                })
                .collect(),
        ),
        type_args: None,
    })
}

pub fn arg(expr: Expr) -> ExprOrSpread {
    ExprOrSpread {
        spread: None,
        expr: Box::new(expr),
    }
}

pub fn obj_lit(props: Vec<PropOrSpread>) -> Expr {
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props,
    })
}

pub fn kv_prop(key: &str, value: Expr) -> PropOrSpread {
    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
        key: PropName::Ident(ident_name(key)),
        value: Box::new(value),
    })))
}

pub fn kv_prop_str(key: &str, value: Expr) -> PropOrSpread {
    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
        key: PropName::Str(Str {
            span: DUMMY_SP,
            value: key.into(),
            raw: None,
        }),
        value: Box::new(value),
    })))
}

pub fn spread_prop(expr: Expr) -> PropOrSpread {
    PropOrSpread::Spread(SpreadElement {
        dot3_token: DUMMY_SP,
        expr: Box::new(expr),
    })
}

pub fn array_lit(elems: Vec<Expr>) -> Expr {
    Expr::Array(ArrayLit {
        span: DUMMY_SP,
        elems: elems
            .into_iter()
            .map(|e| {
                Some(ExprOrSpread {
                    spread: None,
                    expr: Box::new(e),
                })
            })
            .collect(),
    })
}

pub fn paren(expr: Expr) -> Expr {
    Expr::Paren(ParenExpr {
        span: DUMMY_SP,
        expr: Box::new(expr),
    })
}

pub fn as_const(expr: Expr) -> Expr {
    Expr::TsConstAssertion(TsConstAssertion {
        span: DUMMY_SP,
        expr: Box::new(expr),
    })
}

pub fn ts_as_expr(expr: Expr, ty: Box<TsType>) -> Expr {
    Expr::TsAs(TsAsExpr {
        span: DUMMY_SP,
        expr: Box::new(expr),
        type_ann: ty,
    })
}

pub fn cond_expr(test: Expr, cons: Expr, alt: Expr) -> Expr {
    Expr::Cond(CondExpr {
        span: DUMMY_SP,
        test: Box::new(test),
        cons: Box::new(cons),
        alt: Box::new(alt),
    })
}

pub fn bin_expr(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    Expr::Bin(BinExpr {
        span: DUMMY_SP,
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

pub fn not_null_check(expr: Expr) -> Expr {
    bin_expr(expr, BinaryOp::NotEq, null_lit())
}

pub fn unary_not(expr: Expr) -> Expr {
    Expr::Unary(UnaryExpr {
        span: DUMMY_SP,
        op: UnaryOp::Bang,
        arg: Box::new(expr),
    })
}

pub fn assign_expr(target: Expr, value: Expr) -> Expr {
    Expr::Assign(AssignExpr {
        span: DUMMY_SP,
        op: AssignOp::Assign,
        left: AssignTarget::Simple(SimpleAssignTarget::Member(match target {
            Expr::Member(m) => m,
            _ => unreachable!("assign_expr target must be a MemberExpr"),
        })),
        right: Box::new(value),
    })
}

// =============================================================================
// Statements
// =============================================================================

pub fn const_decl(name: &str, init: Expr) -> Stmt {
    Stmt::Decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: DUMMY_SP,
            name: Pat::Ident(binding_ident(name, None, false)),
            init: Some(Box::new(init)),
            definite: false,
        }],
    })))
}

pub fn return_stmt(expr: Option<Expr>) -> Stmt {
    Stmt::Return(ReturnStmt {
        span: DUMMY_SP,
        arg: expr.map(Box::new),
    })
}

pub fn if_stmt(test: Expr, cons: Stmt, alt: Option<Stmt>) -> Stmt {
    Stmt::If(IfStmt {
        span: DUMMY_SP,
        test: Box::new(test),
        cons: Box::new(cons),
        alt: alt.map(Box::new),
    })
}

pub fn throw_stmt(expr: Expr) -> Stmt {
    Stmt::Throw(ThrowStmt {
        span: DUMMY_SP,
        arg: Box::new(expr),
    })
}

pub fn expr_stmt(expr: Expr) -> Stmt {
    Stmt::Expr(ExprStmt {
        span: DUMMY_SP,
        expr: Box::new(expr),
    })
}

pub fn block(stmts: Vec<Stmt>) -> BlockStmt {
    BlockStmt {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        stmts,
    }
}

pub fn block_stmt(stmts: Vec<Stmt>) -> Stmt {
    Stmt::Block(block(stmts))
}

pub fn try_stmt(block_body: BlockStmt, catch_body: Option<BlockStmt>) -> Stmt {
    Stmt::Try(Box::new(TryStmt {
        span: DUMMY_SP,
        block: block_body,
        handler: catch_body.map(|body| CatchClause {
            span: DUMMY_SP,
            param: None,
            body,
        }),
        finalizer: None,
    }))
}

// =============================================================================
// Module Items
// =============================================================================

pub fn import_named(items: Vec<(&str, Option<&str>)>, from: &str, type_only: bool) -> ModuleItem {
    let specifiers = items
        .into_iter()
        .map(|(name, alias)| {
            ImportSpecifier::Named(ImportNamedSpecifier {
                span: DUMMY_SP,
                local: ident(alias.unwrap_or(name)),
                imported: alias.map(|_| ModuleExportName::Ident(ident(name))),
                is_type_only: false,
            })
        })
        .collect();

    ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
        span: DUMMY_SP,
        specifiers,
        src: Box::new(Str {
            span: DUMMY_SP,
            value: from.into(),
            raw: None,
        }),
        type_only,
        with: None,
        phase: ImportPhase::Evaluation,
    }))
}

pub fn export_decl(decl: Decl) -> ModuleItem {
    ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
        span: DUMMY_SP,
        decl,
    }))
}

pub fn export_const_arrow(
    name: &str,
    params: Vec<Pat>,
    ret: Option<Box<TsType>>,
    body_stmts: BlockStmt,
    is_async: bool,
) -> ModuleItem {
    let arrow = ArrowExpr {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        params,
        body: Box::new(BlockStmtOrExpr::BlockStmt(body_stmts)),
        is_async,
        is_generator: false,
        type_params: None,
        return_type: ret.map(ts_type_ann),
    };

    export_decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: DUMMY_SP,
            name: Pat::Ident(binding_ident(name, None, false)),
            init: Some(Box::new(Expr::Arrow(arrow))),
            definite: false,
        }],
    })))
}

pub fn export_function(
    name: &str,
    type_params: Option<Vec<TsTypeParam>>,
    params: Vec<Param>,
    ret: Option<Box<TsType>>,
    body_stmts: BlockStmt,
    is_async: bool,
) -> ModuleItem {
    let tp = type_params.map(|tps| {
        Box::new(TsTypeParamDecl {
            span: DUMMY_SP,
            params: tps,
        })
    });

    export_decl(Decl::Fn(FnDecl {
        ident: ident(name),
        declare: false,
        function: Box::new(Function {
            params,
            decorators: vec![],
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            body: Some(body_stmts),
            is_generator: false,
            is_async,
            type_params: tp,
            return_type: ret.map(ts_type_ann),
        }),
    }))
}

pub fn export_interface(name: &str, props: Vec<TsTypeElement>) -> ModuleItem {
    export_decl(Decl::TsInterface(Box::new(TsInterfaceDecl {
        span: DUMMY_SP,
        id: ident(name),
        declare: false,
        type_params: None,
        extends: vec![],
        body: TsInterfaceBody {
            span: DUMMY_SP,
            body: props,
        },
    })))
}

pub fn export_type_alias(name: &str, ty: Box<TsType>) -> ModuleItem {
    export_decl(Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
        span: DUMMY_SP,
        declare: false,
        id: ident(name),
        type_params: None,
        type_ann: ty,
    })))
}

pub fn export_class(name: &str, super_class: Option<&str>, body: Vec<ClassMember>) -> ModuleItem {
    export_decl(Decl::Class(ClassDecl {
        ident: ident(name),
        declare: false,
        class: Box::new(Class {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            decorators: vec![],
            body,
            super_class: super_class.map(|s| Box::new(ident_expr(s))),
            is_abstract: false,
            type_params: None,
            super_type_params: None,
            implements: vec![],
        }),
    }))
}

pub fn class_prop(name: &str, ty: Box<TsType>) -> ClassMember {
    ClassMember::ClassProp(ClassProp {
        span: DUMMY_SP,
        key: PropName::Ident(ident_name(name)),
        value: None,
        type_ann: Some(ts_type_ann(ty)),
        is_static: false,
        decorators: vec![],
        accessibility: None,
        is_abstract: false,
        is_optional: false,
        is_override: false,
        readonly: false,
        declare: false,
        definite: false,
    })
}

pub fn constructor(params: Vec<ParamOrTsParamProp>, body: BlockStmt) -> ClassMember {
    ClassMember::Constructor(Constructor {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        key: PropName::Ident(ident_name("constructor")),
        params,
        body: Some(body),
        accessibility: None,
        is_optional: false,
    })
}

// =============================================================================
// Parameter helpers
// =============================================================================

pub fn param(name: &str, ty: Option<Box<TsType>>, optional: bool) -> Param {
    Param {
        span: DUMMY_SP,
        decorators: vec![],
        pat: Pat::Ident(binding_ident(name, ty, optional)),
    }
}

pub fn pat_ident(name: &str, ty: Option<Box<TsType>>, optional: bool) -> Pat {
    Pat::Ident(binding_ident(name, ty, optional))
}

pub fn constructor_param(name: &str, ty: Option<Box<TsType>>) -> ParamOrTsParamProp {
    ParamOrTsParamProp::Param(param(name, ty, false))
}

// =============================================================================
// Conversion from custom IR types -> SWC types
// =============================================================================

/// Convert our IR `TsType` to an SWC `TsType`.
pub fn ir_type_to_swc(ty: &ir::TsType) -> Box<TsType> {
    match ty {
        ir::TsType::Primitive(p) => match p {
            ir::TsPrimitive::String => ts_kw!(string),
            ir::TsPrimitive::Number => ts_kw!(number),
            ir::TsPrimitive::Boolean => ts_kw!(boolean),
            ir::TsPrimitive::Null => ts_kw!(null),
            ir::TsPrimitive::Void => ts_kw!(void),
            ir::TsPrimitive::Unknown => ts_kw!(unknown),
        },
        ir::TsType::Array(inner) => {
            let elem = ir_type_to_swc(inner);
            // Wrap union/intersection in parens: (A | B)[]
            match &**inner {
                ir::TsType::Union(_) | ir::TsType::Intersection(_) => ts_array(ts_paren(elem)),
                _ => ts_array(elem),
            }
        }
        ir::TsType::Union(types) => ts_union(types.iter().map(ir_type_to_swc).collect()),
        ir::TsType::Intersection(types) => {
            let parts: Vec<_> = types
                .iter()
                .map(|t| {
                    let swc_t = ir_type_to_swc(t);
                    // Wrap union types in parens within intersection
                    if matches!(t, ir::TsType::Union(_)) {
                        ts_paren(swc_t)
                    } else {
                        swc_t
                    }
                })
                .collect();
            ts_intersection(parts)
        }
        ir::TsType::Object(props) => {
            let members = props
                .iter()
                .map(|p| ts_property_sig(&p.name, ir_type_to_swc(&p.ty), p.optional))
                .collect();
            ts_object_type(members)
        }
        ir::TsType::Record { key, value } => {
            ts_type_ref_with_params("Record", vec![ir_type_to_swc(key), ir_type_to_swc(value)])
        }
        ir::TsType::Literal(lit) => ir_literal_to_swc_type(lit),
        ir::TsType::Ref(name) => ts_type_ref(name),
    }
}

/// Convert our IR `TsLiteral` to an SWC `TsType`.
fn ir_literal_to_swc_type(lit: &ir::TsLiteral) -> Box<TsType> {
    match lit {
        ir::TsLiteral::String(s) => ts_lit_str(s),
        ir::TsLiteral::Number(n) => ts_lit_num(*n),
        ir::TsLiteral::Int(i) => ts_lit_num(*i as f64),
        ir::TsLiteral::Bool(b) => ts_lit_bool(*b),
        ir::TsLiteral::Null => ts_kw!(null),
    }
}

/// Convert our IR `TypeRef` to an SWC `TsType`.
pub fn ir_typeref_to_swc(tr: &TypeRef) -> Box<TsType> {
    match tr {
        TypeRef::Named(name) => ts_type_ref(name),
        TypeRef::Inline(t) => ir_type_to_swc(t),
    }
}

/// Convert our IR `TsTypeDef` to SWC `ModuleItem`(s).
pub fn ir_typedef_to_module_items(td: &ir::TsTypeDef) -> Vec<ModuleItem> {
    match &td.kind {
        ir::TypeDefKind::Interface { properties } => {
            let props = properties
                .iter()
                .map(|p| ts_property_sig(&p.name, ir_type_to_swc(&p.ty), p.optional))
                .collect();
            vec![export_interface(&td.name, props)]
        }
        ir::TypeDefKind::TypeAlias { ty } => {
            vec![export_type_alias(&td.name, ir_type_to_swc(ty))]
        }
        ir::TypeDefKind::ConstEnum { values } => {
            // export const Name = { key: value, ... } as const;
            // Keys come from enum_value_to_key which may pre-quote them with "..."
            // We need to strip those quotes for SWC since SWC handles quoting itself.
            let props: Vec<PropOrSpread> = values
                .iter()
                .map(|(key, val)| {
                    let value = ir_literal_to_expr(val);
                    // If the key was pre-quoted by quote_if_needed (starts/ends with "),
                    // strip the quotes and use PropName::Str with the raw content
                    if key.starts_with('"') && key.ends_with('"') {
                        let raw_key = &key[1..key.len() - 1];
                        // Unescape: the key may contain \" and \\
                        let unescaped = raw_key.replace("\\\"", "\"").replace("\\\\", "\\");
                        kv_prop_str(&unescaped, value)
                    } else {
                        kv_prop(key, value)
                    }
                })
                .collect();
            let const_obj = as_const(obj_lit(props));
            let const_decl_item = export_decl(Decl::Var(Box::new(VarDecl {
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                kind: VarDeclKind::Const,
                declare: false,
                decls: vec![VarDeclarator {
                    span: DUMMY_SP,
                    name: Pat::Ident(binding_ident(&td.name, None, false)),
                    init: Some(Box::new(const_obj)),
                    definite: false,
                }],
            })));

            // export type Name = (typeof Name)[keyof typeof Name];
            let typeof_name = Box::new(TsType::TsTypeQuery(TsTypeQuery {
                span: DUMMY_SP,
                expr_name: TsTypeQueryExpr::TsEntityName(TsEntityName::Ident(ident(&td.name))),
                type_args: None,
            }));
            let keyof_typeof = Box::new(TsType::TsTypeOperator(TsTypeOperator {
                span: DUMMY_SP,
                op: TsTypeOperatorOp::KeyOf,
                type_ann: typeof_name.clone(),
            }));
            let indexed = Box::new(TsType::TsIndexedAccessType(TsIndexedAccessType {
                span: DUMMY_SP,
                readonly: false,
                obj_type: typeof_name,
                index_type: keyof_typeof,
            }));
            let type_alias_item = export_type_alias(&td.name, indexed);

            vec![const_decl_item, type_alias_item]
        }
    }
}

/// Convert our IR literal to an SWC expression.
fn ir_literal_to_expr(lit: &ir::TsLiteral) -> Expr {
    match lit {
        ir::TsLiteral::String(s) => str_lit(s),
        ir::TsLiteral::Number(n) => num_lit(*n),
        ir::TsLiteral::Int(i) => num_lit(*i as f64),
        ir::TsLiteral::Bool(b) => bool_lit(*b),
        ir::TsLiteral::Null => null_lit(),
    }
}
