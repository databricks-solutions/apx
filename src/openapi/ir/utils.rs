//! Common utilities for TypeScript code generation.
//!
//! This module provides shared helper functions used across normalization and printing.

use std::collections::HashSet;
use std::sync::LazyLock;

use super::types::{TsLiteral, TsPrimitive, TsType};
use crate::openapi::spec::EnumValue;

/// TypeScript reserved words that cannot be used as identifiers.
pub static TS_RESERVED_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
        "let",
        "static",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "await",
        "async",
    ]
    .into_iter()
    .collect()
});

/// Check if an identifier needs bracket notation (or quoting) for property/key access.
///
/// Returns true if the name:
/// - Is empty
/// - Doesn't start with a letter, underscore, or dollar sign
/// - Contains characters other than alphanumeric, underscore, or dollar sign
pub fn needs_bracket_notation(name: &str) -> bool {
    name.is_empty()
        || !name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            .unwrap_or(false)
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Escape a string for use in JavaScript/TypeScript string literals.
/// Escapes backslashes and double quotes.
pub fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Quote a string if needed for use as a property key or enum key.
/// Returns the name quoted with escaped special characters if needed,
/// or the original name if it's a valid identifier.
pub fn quote_if_needed(name: &str) -> String {
    if needs_bracket_notation(name) {
        format!("\"{}\"", escape_js_string(name))
    } else {
        name.to_string()
    }
}

/// Format a parameter access expression (e.g., `params.foo` or `params["foo-bar"]`).
///
/// # Arguments
/// * `obj` - The object name (e.g., "params")
/// * `prop` - The property name
/// * `required` - Whether the property is required (affects optional chaining)
pub fn format_param_access(obj: &str, prop: &str, required: bool) -> String {
    if needs_bracket_notation(prop) {
        if required {
            format!("{}[\"{}\"]", obj, escape_js_string(prop))
        } else {
            format!("{}?.[\"{}\"]", obj, escape_js_string(prop))
        }
    } else if required {
        format!("{obj}.{prop}")
    } else {
        format!("{obj}?.{prop}")
    }
}

/// Sanitize an identifier to be a valid TypeScript identifier.
/// - Replaces `-`, `.`, ` ` with separators and converts to camelCase
/// - Prepends `_` if starts with digit
/// - Escapes reserved words with `_` prefix
pub fn sanitize_ts_identifier(name: &str) -> String {
    if name.is_empty() {
        return "_empty".to_string();
    }

    // Split on separators (-, ., space) and convert to camelCase
    let parts: Vec<&str> = name.split(['-', '.', ' ']).collect();

    let mut result = String::new();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First part: keep lowercase
            result.push_str(part);
        } else {
            // Subsequent parts: capitalize first letter
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                result.extend(first.to_uppercase());
                result.extend(chars);
            }
        }
    }

    // If empty after processing, use a default
    if result.is_empty() {
        return "_empty".to_string();
    }

    // Prepend underscore if starts with digit
    if result
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        result = format!("_{result}");
    }

    // Check for reserved words (case-sensitive)
    if TS_RESERVED_WORDS.contains(result.as_str()) {
        result = format!("_{result}");
    }

    result
}

/// Capitalize the first letter of a string.
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

/// Convert a string to snake_case (for comparison purposes).
pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Convert an OpenAPI enum value to a TypeScript literal.
pub fn enum_value_to_literal(v: &EnumValue) -> TsLiteral {
    match v {
        EnumValue::String(s) => TsLiteral::String(s.clone()),
        EnumValue::Integer(n) => TsLiteral::Int(*n),
        EnumValue::Float(f) => TsLiteral::Number(*f),
        EnumValue::Bool(b) => TsLiteral::Bool(*b),
        EnumValue::Null => TsLiteral::Null,
    }
}

/// Generate a key name for an enum value (used in const enum objects).
pub fn enum_value_to_key(v: &EnumValue, index: usize) -> String {
    match v {
        EnumValue::String(s) => quote_if_needed(s),
        EnumValue::Integer(n) => format!("VALUE_{n}"),
        EnumValue::Float(_) => format!("VALUE_{index}"),
        EnumValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        EnumValue::Null => "NULL".to_string(),
    }
}

/// Create a `Record<string, T>` type.
pub fn make_string_record(value_type: TsType) -> TsType {
    TsType::Record {
        key: Box::new(TsType::Primitive(TsPrimitive::String)),
        value: Box::new(value_type),
    }
}

/// Create a `Record<string, unknown>` type (common default for additionalProperties: true).
pub fn make_unknown_record() -> TsType {
    make_string_record(TsType::Primitive(TsPrimitive::Unknown))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_bracket_notation() {
        // Valid identifiers
        assert!(!needs_bracket_notation("foo"));
        assert!(!needs_bracket_notation("_foo"));
        assert!(!needs_bracket_notation("$foo"));
        assert!(!needs_bracket_notation("foo123"));
        assert!(!needs_bracket_notation("camelCase"));

        // Need bracket notation
        assert!(needs_bracket_notation(""));
        assert!(needs_bracket_notation("123foo"));
        assert!(needs_bracket_notation("foo-bar"));
        assert!(needs_bracket_notation("foo.bar"));
        assert!(needs_bracket_notation("foo bar"));
        assert!(needs_bracket_notation("foo:bar"));
    }

    #[test]
    fn test_escape_js_string() {
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string("hel\"lo"), "hel\\\"lo");
        assert_eq!(escape_js_string("hel\\lo"), "hel\\\\lo");
        assert_eq!(escape_js_string("a\\\"b"), "a\\\\\\\"b");
    }

    #[test]
    fn test_quote_if_needed() {
        assert_eq!(quote_if_needed("foo"), "foo");
        assert_eq!(quote_if_needed("foo-bar"), "\"foo-bar\"");
        assert_eq!(quote_if_needed("123"), "\"123\"");
    }

    #[test]
    fn test_format_param_access() {
        assert_eq!(format_param_access("params", "foo", true), "params.foo");
        assert_eq!(format_param_access("params", "foo", false), "params?.foo");
        assert_eq!(
            format_param_access("params", "foo-bar", true),
            "params[\"foo-bar\"]"
        );
        assert_eq!(
            format_param_access("params", "foo-bar", false),
            "params?.[\"foo-bar\"]"
        );
    }

    #[test]
    fn test_sanitize_ts_identifier() {
        assert_eq!(sanitize_ts_identifier("foo"), "foo");
        assert_eq!(sanitize_ts_identifier("foo-bar"), "fooBar");
        assert_eq!(sanitize_ts_identifier("foo.bar"), "fooBar");
        assert_eq!(sanitize_ts_identifier("123foo"), "_123foo");
        assert_eq!(sanitize_ts_identifier("delete"), "_delete");
        assert_eq!(sanitize_ts_identifier("class"), "_class");
    }

    #[test]
    fn test_capitalize_first() {
        assert_eq!(capitalize_first("foo"), "Foo");
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("a"), "A");
        assert_eq!(capitalize_first("ABC"), "ABC");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("fooBar"), "foo_bar");
        assert_eq!(to_snake_case("FooBar"), "foo_bar");
        assert_eq!(to_snake_case("foo"), "foo");
        assert_eq!(to_snake_case("itemId"), "item_id");
    }
}
