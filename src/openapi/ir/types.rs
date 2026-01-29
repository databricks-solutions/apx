//! TypeScript IR types for code generation.
//!
//! This module defines the TypeScript type system representation:
//! - TsType: Types (primitives, arrays, unions, objects, etc.)
//! - TsExpr: Expressions (identifiers, calls, arrows, etc.)
//! - TsLiteral: Literal values (strings, numbers, booleans)

// Allow dead code for IR types that are part of the design but not yet fully utilized.
#![allow(dead_code)]

/// Reference to a type - either inline or named
#[derive(Debug, Clone)]
pub enum TypeRef {
    /// Reference to a named type: "Item", "User"
    Named(String),
    /// Inline type definition
    Inline(Box<TsType>),
}

impl TypeRef {
    /// Check if this type reference is an array type
    pub fn is_array(&self) -> bool {
        match self {
            TypeRef::Named(_) => false, // Named types could be arrays but we can't know
            TypeRef::Inline(t) => t.is_array(),
        }
    }

    /// Convert this TypeRef to a TsType (resolving named references to TsType::Ref)
    pub fn to_ts_type(&self) -> TsType {
        match self {
            TypeRef::Named(name) => TsType::Ref(name.clone()),
            TypeRef::Inline(t) => (**t).clone(),
        }
    }
}

impl TsType {
    /// Check if this type is an array type (including nullable arrays)
    pub fn is_array(&self) -> bool {
        match self {
            TsType::Array(_) => true,
            TsType::Union(types) => {
                // Check if any non-null type in the union is an array
                types
                    .iter()
                    .any(|t| !matches!(t, TsType::Primitive(TsPrimitive::Null)) && t.is_array())
            }
            _ => false,
        }
    }
}

/// TypeScript type representation
#[derive(Debug, Clone)]
pub enum TsType {
    /// Primitive types: string, number, boolean, null, void, unknown
    Primitive(TsPrimitive),
    /// Array type: T[]
    Array(Box<TsType>),
    /// Union type: A | B | C
    Union(Vec<TsType>),
    /// Intersection type: A & B & C
    Intersection(Vec<TsType>),
    /// Object type: { foo: string; bar?: number }
    Object(Vec<TsProp>),
    /// Record type: Record<K, V>
    Record {
        key: Box<TsType>,
        value: Box<TsType>,
    },
    /// Literal type: "foo", 42, true
    Literal(TsLiteral),
    /// Named type reference (shorthand for TypeRef::Named in type position)
    Ref(String),
}

/// TypeScript primitive types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsPrimitive {
    String,
    Number,
    Boolean,
    Null,
    Void,
    Unknown,
}

/// Object property definition
#[derive(Debug, Clone)]
pub struct TsProp {
    pub name: String,
    pub ty: TsType,
    pub optional: bool,
}

/// TypeScript literal values
#[derive(Debug, Clone)]
pub enum TsLiteral {
    String(String),
    Number(f64),
    Int(i64),
    Bool(bool),
    Null,
}

/// TypeScript expression
#[derive(Debug, Clone)]
pub enum TsExpr {
    /// Identifier: foo
    Ident(String),
    /// Literal value: "bar", 42
    Literal(TsLiteral),
    /// Function call: foo(a, b)
    Call {
        callee: Box<TsExpr>,
        args: Vec<TsExpr>,
    },
    /// Arrow function: (x) => x.foo
    Arrow {
        params: Vec<TsParam>,
        body: Box<TsExpr>,
    },
    /// Object literal: { a: 1, b: 2 }
    Object(Vec<(String, TsExpr)>),
    /// Member access: foo.bar
    Member { object: Box<TsExpr>, prop: String },
    /// Template literal: `${foo}/bar`
    Template(Vec<TemplatePart>),
    /// Await expression: await fetch()
    Await(Box<TsExpr>),
    /// Spread: ...options
    Spread(Box<TsExpr>),
    /// Conditional: a !== undefined
    BinOp {
        left: Box<TsExpr>,
        op: BinOp,
        right: Box<TsExpr>,
    },
    /// Optional chaining member access: foo?.bar
    OptionalMember { object: Box<TsExpr>, prop: String },
    /// new URL(...)
    New {
        callee: Box<TsExpr>,
        args: Vec<TsExpr>,
    },
    /// Throw expression (for error handling)
    Throw(Box<TsExpr>),
    /// Ternary/conditional: cond ? a : b
    Ternary {
        cond: Box<TsExpr>,
        then_expr: Box<TsExpr>,
        else_expr: Box<TsExpr>,
    },
    /// Index/bracket access: obj[key]
    Index {
        object: Box<TsExpr>,
        index: Box<TsExpr>,
    },
    /// Array literal: [a, b, c]
    Array(Vec<TsExpr>),
    /// Type cast: expr as Type
    Cast { expr: Box<TsExpr>, ty: TsType },
    /// Raw code that doesn't fit the AST
    Raw(String),
}

/// Binary operators
#[derive(Debug, Clone, Copy)]
pub enum BinOp {
    NotEqual,
    StrictNotEqual,
}

/// Function parameter
#[derive(Debug, Clone)]
pub struct TsParam {
    pub name: String,
    pub ty: Option<TsType>,
    pub optional: bool,
}

/// Template literal part
#[derive(Debug, Clone)]
pub enum TemplatePart {
    /// Static string part
    Static(String),
    /// Dynamic expression part: ${expr}
    Dynamic(TsExpr),
}

// =============================================================================
// Module-Level IR (for printer)
// =============================================================================

/// Import statement
#[derive(Debug, Clone)]
pub struct TsImport {
    /// Items to import
    pub items: Vec<ImportItem>,
    /// Module path
    pub from: String,
    /// Whether this is a type-only import
    pub type_only: bool,
}

/// Import item
#[derive(Debug, Clone)]
pub struct ImportItem {
    pub name: String,
    pub alias: Option<String>,
}

/// Type definition kind
#[derive(Debug, Clone)]
pub enum TypeDefKind {
    /// interface Foo { ... }
    Interface { properties: Vec<TsProp> },
    /// type Foo = ...
    TypeAlias { ty: TsType },
    /// const Foo = { ... } as const; type Foo = ...
    ConstEnum { values: Vec<(String, TsLiteral)> },
}

/// Type definition
#[derive(Debug, Clone)]
pub struct TsTypeDef {
    pub name: String,
    pub kind: TypeDefKind,
}

/// Statement in a function body
#[derive(Debug, Clone)]
pub enum TsStmt {
    /// const/let declaration
    VarDecl {
        kind: VarKind,
        name: String,
        ty: Option<TsType>,
        init: TsExpr,
    },
    /// Expression statement
    Expr(TsExpr),
    /// Return statement
    Return(Option<TsExpr>),
    /// If statement
    If {
        cond: TsExpr,
        then_body: Vec<TsStmt>,
        else_body: Option<Vec<TsStmt>>,
    },
    /// Throw statement
    Throw(TsExpr),
    /// Raw code block (for complex patterns that don't fit the AST)
    Raw(String),
}

/// Variable declaration kind
#[derive(Debug, Clone, Copy)]
pub enum VarKind {
    Const,
    Let,
}

/// Function definition
#[derive(Debug, Clone)]
pub struct TsFunction {
    pub name: String,
    pub type_params: Vec<String>,
    pub params: Vec<TsParam>,
    pub return_type: Option<TsType>,
    pub body: Vec<TsStmt>,
    pub is_async: bool,
    pub is_export: bool,
    pub is_arrow: bool,
}

/// Complete TypeScript module
#[derive(Debug, Clone)]
pub struct TsModule {
    pub imports: Vec<TsImport>,
    pub types: Vec<TsTypeDef>,
    pub functions: Vec<TsFunction>,
}
