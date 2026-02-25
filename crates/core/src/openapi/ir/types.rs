//! TypeScript IR types for type-level code generation.
//!
//! This module defines the domain-level type system representation used by
//! `normalize.rs` and the SWC codegen. Only type-related IR is kept here;
//! all code-level AST (expressions, statements, functions) is now handled
//! directly by SWC's `swc_ecma_ast`.

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
