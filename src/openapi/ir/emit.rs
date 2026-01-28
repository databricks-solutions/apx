//! TypeScript code emission via the Emit trait.
//!
//! This module provides a trait-based approach to converting TypeScript AST nodes
//! to string representations. Each AST type implements `Emit` for clean, composable
//! code generation.

use super::types::{
    BinOp, ImportItem, TemplatePart, TsExpr, TsFunction, TsImport, TsLiteral, TsModule, TsParam,
    TsPrimitive, TsProp, TsStmt, TsType, TsTypeDef, TypeDefKind, TypeRef, VarKind,
};
use super::utils::quote_if_needed;

/// Trait for emitting TypeScript code from AST nodes.
pub trait Emit {
    /// Convert the AST node to its TypeScript string representation.
    fn emit(&self) -> String;
}

// =============================================================================
// Primitive Types
// =============================================================================

impl Emit for TsPrimitive {
    fn emit(&self) -> String {
        match self {
            TsPrimitive::String => "string".to_string(),
            TsPrimitive::Number => "number".to_string(),
            TsPrimitive::Boolean => "boolean".to_string(),
            TsPrimitive::Null => "null".to_string(),
            TsPrimitive::Void => "void".to_string(),
            TsPrimitive::Unknown => "unknown".to_string(),
        }
    }
}

impl Emit for TsLiteral {
    fn emit(&self) -> String {
        match self {
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
}

// =============================================================================
// Types
// =============================================================================

impl Emit for TsType {
    fn emit(&self) -> String {
        match self {
            TsType::Primitive(p) => p.emit(),
            TsType::Array(inner) => {
                let inner_str = inner.emit();
                // Wrap complex types in parentheses
                if matches!(**inner, TsType::Union(_) | TsType::Intersection(_)) {
                    format!("({})[]", inner_str)
                } else {
                    format!("{}[]", inner_str)
                }
            }
            TsType::Union(types) => types.iter().map(|t| t.emit()).collect::<Vec<_>>().join(" | "),
            TsType::Intersection(types) => types
                .iter()
                .map(|t| {
                    let s = t.emit();
                    if matches!(t, TsType::Union(_)) {
                        format!("({})", s)
                    } else {
                        s
                    }
                })
                .collect::<Vec<_>>()
                .join(" & "),
            TsType::Object(props) => {
                if props.is_empty() {
                    "{}".to_string()
                } else {
                    let parts: Vec<_> = props.iter().map(|p| p.emit()).collect();
                    format!("{{ {} }}", parts.join("; "))
                }
            }
            TsType::Record { key, value } => {
                format!("Record<{}, {}>", key.emit(), value.emit())
            }
            TsType::Literal(lit) => lit.emit(),
            TsType::Ref(name) => name.clone(),
        }
    }
}

impl Emit for TsProp {
    fn emit(&self) -> String {
        let key = quote_if_needed(&self.name);
        let opt = if self.optional { "?" } else { "" };
        format!("{}{}: {}", key, opt, self.ty.emit())
    }
}

impl Emit for TypeRef {
    fn emit(&self) -> String {
        match self {
            TypeRef::Named(n) => n.clone(),
            TypeRef::Inline(t) => t.emit(),
        }
    }
}

// =============================================================================
// Type Definitions
// =============================================================================

impl Emit for TsTypeDef {
    fn emit(&self) -> String {
        match &self.kind {
            TypeDefKind::Interface { properties } => {
                let mut output = format!("export interface {} {{\n", self.name);
                for prop in properties {
                    let key = quote_if_needed(&prop.name);
                    let opt = if prop.optional { "?" } else { "" };
                    output.push_str(&format!("  {}{}: {};\n", key, opt, prop.ty.emit()));
                }
                output.push_str("}\n");
                output
            }
            TypeDefKind::TypeAlias { ty } => {
                format!("export type {} = {};\n", self.name, ty.emit())
            }
            TypeDefKind::ConstEnum { values } => {
                let mut output = format!("export const {} = {{\n", self.name);
                for (key, value) in values {
                    output.push_str(&format!("  {}: {},\n", key, value.emit()));
                }
                output.push_str("} as const;\n\n");
                output.push_str(&format!(
                    "export type {} = (typeof {})[keyof typeof {}];\n",
                    self.name, self.name, self.name
                ));
                output
            }
        }
    }
}

// =============================================================================
// Expressions
// =============================================================================

impl Emit for BinOp {
    fn emit(&self) -> String {
        match self {
            BinOp::NotEqual => "!=".to_string(),
            BinOp::StrictNotEqual => "!==".to_string(),
        }
    }
}

impl Emit for TsExpr {
    fn emit(&self) -> String {
        match self {
            TsExpr::Ident(name) => name.clone(),
            TsExpr::Literal(lit) => lit.emit(),
            TsExpr::Call { callee, args } => {
                let args_str = args.iter().map(|a| a.emit()).collect::<Vec<_>>().join(", ");
                format!("{}({})", callee.emit(), args_str)
            }
            TsExpr::Arrow { params, body } => {
                let params_str = params.iter().map(|p| p.emit()).collect::<Vec<_>>().join(", ");
                format!("({}) => {}", params_str, body.emit())
            }
            TsExpr::Object(props) => {
                if props.is_empty() {
                    "{}".to_string()
                } else {
                    let parts: Vec<_> = props
                        .iter()
                        .map(|(k, v)| {
                            let key = quote_if_needed(k);
                            format!("{}: {}", key, v.emit())
                        })
                        .collect();
                    format!("{{ {} }}", parts.join(", "))
                }
            }
            TsExpr::Member { object, prop } => {
                format!("{}.{}", object.emit(), prop)
            }
            TsExpr::Template(parts) => {
                let content: String = parts
                    .iter()
                    .map(|p| match p {
                        TemplatePart::Static(s) => s.clone(),
                        TemplatePart::Dynamic(e) => format!("${{{}}}", e.emit()),
                    })
                    .collect();
                format!("`{}`", content)
            }
            TsExpr::Await(expr) => {
                format!("await {}", expr.emit())
            }
            TsExpr::Spread(expr) => {
                format!("...{}", expr.emit())
            }
            TsExpr::BinOp { left, op, right } => {
                format!("{} {} {}", left.emit(), op.emit(), right.emit())
            }
            TsExpr::OptionalMember { object, prop } => {
                format!("{}?.{}", object.emit(), prop)
            }
            TsExpr::New { callee, args } => {
                let args_str = args.iter().map(|a| a.emit()).collect::<Vec<_>>().join(", ");
                format!("new {}({})", callee.emit(), args_str)
            }
            TsExpr::Throw(expr) => {
                format!("throw {}", expr.emit())
            }
            TsExpr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                format!("{} ? {} : {}", cond.emit(), then_expr.emit(), else_expr.emit())
            }
            TsExpr::Index { object, index } => {
                format!("{}[{}]", object.emit(), index.emit())
            }
            TsExpr::Array(items) => {
                let items_str = items.iter().map(|i| i.emit()).collect::<Vec<_>>().join(", ");
                format!("[{}]", items_str)
            }
            TsExpr::Cast { expr, ty } => {
                format!("{} as {}", expr.emit(), ty.emit())
            }
            TsExpr::Raw(code) => code.clone(),
        }
    }
}

impl Emit for TsParam {
    fn emit(&self) -> String {
        let opt = if self.optional { "?" } else { "" };
        match &self.ty {
            Some(ty) => format!("{}{}: {}", self.name, opt, ty.emit()),
            None => format!("{}{}", self.name, opt),
        }
    }
}

// =============================================================================
// Statements
// =============================================================================

impl Emit for VarKind {
    fn emit(&self) -> String {
        match self {
            VarKind::Const => "const".to_string(),
            VarKind::Let => "let".to_string(),
        }
    }
}

impl Emit for TsStmt {
    fn emit(&self) -> String {
        self.emit_indented(1)
    }
}

impl TsStmt {
    /// Emit with specified indentation level (2 spaces per level)
    pub fn emit_indented(&self, indent: usize) -> String {
        let prefix = "  ".repeat(indent);
        match self {
            TsStmt::VarDecl { kind, name, ty, init } => {
                let ty_str = ty.as_ref().map(|t| format!(": {}", t.emit())).unwrap_or_default();
                format!("{}{} {}{} = {};\n", prefix, kind.emit(), name, ty_str, init.emit())
            }
            TsStmt::Expr(expr) => {
                format!("{}{};\n", prefix, expr.emit())
            }
            TsStmt::Return(expr) => match expr {
                Some(e) => format!("{}return {};\n", prefix, e.emit()),
                None => format!("{}return;\n", prefix),
            },
            TsStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let mut output = format!("{}if ({}) {{\n", prefix, cond.emit());
                for stmt in then_body {
                    output.push_str(&stmt.emit_indented(indent + 1));
                }
                if let Some(else_stmts) = else_body {
                    output.push_str(&format!("{}}} else {{\n", prefix));
                    for stmt in else_stmts {
                        output.push_str(&stmt.emit_indented(indent + 1));
                    }
                }
                output.push_str(&format!("{}}}\n", prefix));
                output
            }
            TsStmt::Throw(expr) => {
                format!("{}throw {};\n", prefix, expr.emit())
            }
            TsStmt::Raw(code) => {
                // Raw code is emitted as-is, with proper indentation for each line
                code.lines()
                    .map(|line| {
                        if line.is_empty() {
                            "\n".to_string()
                        } else {
                            format!("{}{}\n", prefix, line)
                        }
                    })
                    .collect()
            }
        }
    }
}

// =============================================================================
// Functions
// =============================================================================

impl Emit for TsFunction {
    fn emit(&self) -> String {
        // Special case: empty name with Raw body = just emit the raw content
        // This is used for things like the ApiError class that don't fit the function AST
        if self.name.is_empty() {
            let mut output = String::new();
            for stmt in &self.body {
                if let TsStmt::Raw(code) = stmt {
                    output.push_str(code);
                    output.push('\n');
                } else {
                    output.push_str(&stmt.emit_indented(0));
                }
            }
            return output;
        }

        let mut output = String::new();

        // Export keyword
        if self.is_export {
            output.push_str("export ");
        }

        // Type parameters
        let type_params_str = if self.type_params.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.type_params.join(", "))
        };

        // Parameters
        let params_str = self.params.iter().map(|p| p.emit()).collect::<Vec<_>>().join(", ");

        // Return type
        let return_type_str = self
            .return_type
            .as_ref()
            .map(|t| format!(": {}", t.emit()))
            .unwrap_or_default();

        // Async modifier
        let async_str = if self.is_async { "async " } else { "" };

        if self.is_arrow {
            // Arrow function: export const name = async (...): Type => { ... }
            output.push_str(&format!(
                "const {} = {}({}){}",
                self.name, async_str, params_str, return_type_str
            ));
            if self.body.is_empty() {
                output.push_str(" => {};\n");
            } else {
                output.push_str(" => {\n");
                for stmt in &self.body {
                    output.push_str(&stmt.emit_indented(1));
                }
                output.push_str("};\n");
            }
        } else {
            // Regular function: export function name<T>(...): Type { ... }
            output.push_str(&format!(
                "{}function {}{}({}){}",
                async_str, self.name, type_params_str, params_str, return_type_str
            ));
            if self.body.is_empty() {
                output.push_str(" {}\n");
            } else {
                output.push_str(" {\n");
                for stmt in &self.body {
                    output.push_str(&stmt.emit_indented(1));
                }
                output.push_str("}\n");
            }
        }

        output
    }
}

// =============================================================================
// Imports
// =============================================================================

impl Emit for ImportItem {
    fn emit(&self) -> String {
        match &self.alias {
            Some(alias) => format!("{} as {}", self.name, alias),
            None => self.name.clone(),
        }
    }
}

impl Emit for TsImport {
    fn emit(&self) -> String {
        let items_str = self.items.iter().map(|i| i.emit()).collect::<Vec<_>>().join(", ");
        let type_keyword = if self.type_only { "type " } else { "" };
        format!("import {}{{ {} }} from \"{}\";\n", type_keyword, items_str, self.from)
    }
}

// =============================================================================
// Module
// =============================================================================

impl Emit for TsModule {
    fn emit(&self) -> String {
        let mut output = String::new();

        // Emit imports
        for import in &self.imports {
            output.push_str(&import.emit());
        }

        if !self.imports.is_empty() {
            output.push('\n');
        }

        // Emit type definitions
        for type_def in &self.types {
            output.push_str(&type_def.emit());
            output.push('\n');
        }

        // Emit functions
        for func in &self.functions {
            output.push_str(&func.emit());
            output.push('\n');
        }

        output
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_primitive() {
        assert_eq!(TsPrimitive::String.emit(), "string");
        assert_eq!(TsPrimitive::Number.emit(), "number");
        assert_eq!(TsPrimitive::Boolean.emit(), "boolean");
        assert_eq!(TsPrimitive::Null.emit(), "null");
        assert_eq!(TsPrimitive::Void.emit(), "void");
        assert_eq!(TsPrimitive::Unknown.emit(), "unknown");
    }

    #[test]
    fn test_emit_literal() {
        assert_eq!(TsLiteral::String("hello".into()).emit(), "\"hello\"");
        assert_eq!(TsLiteral::String("say \"hi\"".into()).emit(), "\"say \\\"hi\\\"\"");
        assert_eq!(TsLiteral::Number(3.14).emit(), "3.14");
        assert_eq!(TsLiteral::Int(42).emit(), "42");
        assert_eq!(TsLiteral::Bool(true).emit(), "true");
        assert_eq!(TsLiteral::Null.emit(), "null");
    }

    #[test]
    fn test_emit_array_type() {
        let ty = TsType::Array(Box::new(TsType::Primitive(TsPrimitive::String)));
        assert_eq!(ty.emit(), "string[]");
    }

    #[test]
    fn test_emit_union_array() {
        // (string | null)[] - union inside array needs parens
        let inner = TsType::Union(vec![
            TsType::Primitive(TsPrimitive::String),
            TsType::Primitive(TsPrimitive::Null),
        ]);
        let ty = TsType::Array(Box::new(inner));
        assert_eq!(ty.emit(), "(string | null)[]");
    }

    #[test]
    fn test_emit_record_type() {
        let ty = TsType::Record {
            key: Box::new(TsType::Primitive(TsPrimitive::String)),
            value: Box::new(TsType::Primitive(TsPrimitive::Unknown)),
        };
        assert_eq!(ty.emit(), "Record<string, unknown>");
    }

    #[test]
    fn test_emit_object_type() {
        let ty = TsType::Object(vec![
            TsProp {
                name: "id".into(),
                ty: TsType::Primitive(TsPrimitive::Number),
                optional: false,
            },
            TsProp {
                name: "name".into(),
                ty: TsType::Primitive(TsPrimitive::String),
                optional: true,
            },
        ]);
        assert_eq!(ty.emit(), "{ id: number; name?: string }");
    }

    #[test]
    fn test_emit_type_def_interface() {
        let def = TsTypeDef {
            name: "Item".into(),
            kind: TypeDefKind::Interface {
                properties: vec![
                    TsProp {
                        name: "id".into(),
                        ty: TsType::Primitive(TsPrimitive::Number),
                        optional: false,
                    },
                    TsProp {
                        name: "name".into(),
                        ty: TsType::Primitive(TsPrimitive::String),
                        optional: false,
                    },
                ],
            },
        };
        let expected = "export interface Item {\n  id: number;\n  name: string;\n}\n";
        assert_eq!(def.emit(), expected);
    }

    #[test]
    fn test_emit_type_def_alias() {
        let def = TsTypeDef {
            name: "ID".into(),
            kind: TypeDefKind::TypeAlias {
                ty: TsType::Primitive(TsPrimitive::String),
            },
        };
        assert_eq!(def.emit(), "export type ID = string;\n");
    }

    #[test]
    fn test_emit_import() {
        let import = TsImport {
            items: vec![
                ImportItem { name: "useQuery".into(), alias: None },
                ImportItem { name: "useMutation".into(), alias: None },
            ],
            from: "@tanstack/react-query".into(),
            type_only: false,
        };
        assert_eq!(import.emit(), "import { useQuery, useMutation } from \"@tanstack/react-query\";\n");
    }

    #[test]
    fn test_emit_type_import() {
        let import = TsImport {
            items: vec![ImportItem { name: "UseQueryOptions".into(), alias: None }],
            from: "@tanstack/react-query".into(),
            type_only: true,
        };
        assert_eq!(
            import.emit(),
            "import type { UseQueryOptions } from \"@tanstack/react-query\";\n"
        );
    }

    #[test]
    fn test_emit_arrow_function() {
        let func = TsFunction {
            name: "fetchData".into(),
            type_params: vec![],
            params: vec![],
            return_type: Some(TsType::Ref("Promise<void>".into())),
            body: vec![TsStmt::Return(None)],
            is_async: true,
            is_export: true,
            is_arrow: true,
        };
        let result = func.emit();
        assert!(result.contains("export const fetchData = async (): Promise<void> => {"));
        assert!(result.contains("return;"));
    }

    #[test]
    fn test_emit_regular_function() {
        let func = TsFunction {
            name: "useItem".into(),
            type_params: vec!["TData".into()],
            params: vec![TsParam {
                name: "id".into(),
                ty: Some(TsType::Primitive(TsPrimitive::Number)),
                optional: false,
            }],
            return_type: None,
            body: vec![],
            is_async: false,
            is_export: true,
            is_arrow: false,
        };
        let result = func.emit();
        assert!(result.contains("export function useItem<TData>(id: number)"));
    }
}
