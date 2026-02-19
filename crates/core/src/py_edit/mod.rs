//! Python source code editing via AST analysis.
//!
//! Uses `ruff_python_parser` to parse Python source into an AST, then performs
//! **text-based** insertions at byte offsets derived from the AST nodes.
//! This preserves original formatting and comments.

mod find;
mod splice;

use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

/// Errors returned by py_edit operations.
#[derive(Debug, thiserror::Error)]
pub enum PyEditError {
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Already present: {0}")]
    AlreadyPresent(String),
    #[error("Not found: {0}")]
    NotFound(String),
}

/// Add an import statement after the last existing import.
///
/// Returns `Err(AlreadyPresent)` if the exact import already exists.
/// If the file has no imports, the statement is inserted at the top of the file
/// (after any module docstring or `from __future__` imports).
pub fn add_import(source: &str, import_stmt: &str) -> Result<String, PyEditError> {
    let trimmed = import_stmt.trim();

    if find::import_exists(source, trimmed) {
        return Err(PyEditError::AlreadyPresent(trimmed.to_string()));
    }

    let parsed = parse_module(source).map_err(|e| PyEditError::Parse(e.to_string()))?;
    let stmts = parsed.suite();

    let insert_offset = find::find_last_import_end(stmts).unwrap_or(0);

    let insertion = format!("\n{trimmed}");
    Ok(splice::insert_at(source, insert_offset, &insertion))
}

/// Add a member to a class body (indentation is auto-detected from existing members).
///
/// Returns `Err(NotFound)` if the class is not found.
/// Returns `Err(AlreadyPresent)` if `member_code` already appears in the source.
pub fn add_class_member(
    source: &str,
    class_name: &str,
    member_code: &str,
) -> Result<String, PyEditError> {
    let trimmed = member_code.trim();

    let parsed = parse_module(source).map_err(|e| PyEditError::Parse(e.to_string()))?;
    let stmts = parsed.suite();

    let class_def = find::find_class(stmts, class_name)
        .ok_or_else(|| PyEditError::NotFound(format!("class {class_name}")))?;

    // Check if member already exists (simple text check)
    let indent = find::class_body_indent(source, class_def);
    let indented = splice::indent_block(trimmed, &indent);
    if source.contains(indented.trim()) {
        return Err(PyEditError::AlreadyPresent(trimmed.to_string()));
    }

    let body_end = find::class_body_end(class_def);
    let insertion = format!("\n{indented}\n");
    Ok(splice::insert_at(source, body_end, &insertion))
}

/// Add or extend a keyword argument on a function call.
///
/// - If the kwarg doesn't exist: adds `kwarg_name=[kwarg_value]`
/// - If the kwarg exists with a list value: appends `kwarg_value` to the list
///
/// Returns `Err(NotFound)` if the call is not found.
/// Returns `Err(AlreadyPresent)` if `kwarg_value` already appears in the kwarg's list.
pub fn add_call_keyword(
    source: &str,
    call_target: &str,
    kwarg_name: &str,
    kwarg_value: &str,
) -> Result<String, PyEditError> {
    let parsed = parse_module(source).map_err(|e| PyEditError::Parse(e.to_string()))?;
    let stmts = parsed.suite();

    let call_info = find::find_call(stmts, call_target)
        .ok_or_else(|| PyEditError::NotFound(format!("call to {call_target}")))?;

    // Check if the kwarg already exists
    if let Some(kw) = call_info.keywords.iter().find(|k| k.name == kwarg_name) {
        // Kwarg exists — check if it's a list and append
        if let Some(list_range) = &kw.list_range {
            // Extract the list text to check for duplicates
            let list_text = &source[usize::from(list_range.start())..usize::from(list_range.end())];
            if list_text.contains(kwarg_value) {
                return Err(PyEditError::AlreadyPresent(format!(
                    "{kwarg_value} in {kwarg_name}"
                )));
            }
            // Insert before the closing `]`
            let insert_pos = usize::from(list_range.end()) - 1;
            let insertion = format!(", {kwarg_value}");
            return Ok(splice::insert_at(source, insert_pos, &insertion));
        }
        // Kwarg exists but is not a list — cannot extend, return error
        return Err(PyEditError::AlreadyPresent(format!(
            "kwarg {kwarg_name} exists but is not a list"
        )));
    }

    // Kwarg doesn't exist — add it
    let insert_pos = call_info.args_end;
    // Determine if we need a comma before the new kwarg
    let before_paren = source[..insert_pos].trim_end();
    let needs_comma = !before_paren.ends_with('(') && !before_paren.ends_with(',');
    let prefix = if needs_comma { ", " } else { "" };
    let insertion = format!("{prefix}{kwarg_name}=[{kwarg_value}]");
    Ok(splice::insert_at(source, insert_pos, &insertion))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // --- add_import tests ---

    #[test]
    fn test_add_import_basic() {
        let source = "import os\nimport sys\n\nx = 1\n";
        let result = add_import(source, "from pathlib import Path").unwrap();
        assert!(result.contains("from pathlib import Path"));
        // Should be after the last import
        let path_pos = result.find("from pathlib import Path").unwrap();
        let sys_pos = result.find("import sys").unwrap();
        assert!(path_pos > sys_pos);
    }

    #[test]
    fn test_add_import_already_present() {
        let source = "import os\nimport sys\n";
        let err = add_import(source, "import os").unwrap_err();
        assert!(matches!(err, PyEditError::AlreadyPresent(_)));
    }

    #[test]
    fn test_add_import_empty_file() {
        let source = "";
        let result = add_import(source, "import os").unwrap();
        assert!(result.contains("import os"));
    }

    #[test]
    fn test_add_import_no_existing_imports() {
        let source = "x = 1\ny = 2\n";
        let result = add_import(source, "import os").unwrap();
        assert!(result.starts_with("\nimport os"));
    }

    #[test]
    fn test_add_import_from_import() {
        let source = "from os import path\n\nx = 1\n";
        let result = add_import(source, "from sys import argv").unwrap();
        assert!(result.contains("from sys import argv"));
    }

    // --- add_class_member tests ---

    #[test]
    fn test_add_class_member_basic() {
        let source = "class Foo:\n    x: int = 1\n    y: int = 2\n";
        let result = add_class_member(source, "Foo", "z: int = 3").unwrap();
        assert!(result.contains("    z: int = 3"));
    }

    #[test]
    fn test_add_class_member_not_found() {
        let source = "class Foo:\n    x: int = 1\n";
        let err = add_class_member(source, "Bar", "z: int = 3").unwrap_err();
        assert!(matches!(err, PyEditError::NotFound(_)));
    }

    #[test]
    fn test_add_class_member_already_present() {
        let source = "class Foo:\n    x: int = 1\n";
        let err = add_class_member(source, "Foo", "x: int = 1").unwrap_err();
        assert!(matches!(err, PyEditError::AlreadyPresent(_)));
    }

    #[test]
    fn test_add_class_member_with_docstring() {
        let source = "class Deps:\n    \"\"\"Dependencies.\"\"\"\n\n    Client: int = 1\n";
        let result = add_class_member(source, "Deps", "Session: int = 2").unwrap();
        assert!(result.contains("    Session: int = 2"));
    }

    // --- add_call_keyword tests ---

    #[test]
    fn test_add_call_keyword_new() {
        let source = "app = create_app(routers=[router])\n";
        let result =
            add_call_keyword(source, "create_app", "lifespans", "lakebase_lifespan").unwrap();
        assert!(result.contains("lifespans=[lakebase_lifespan]"));
    }

    #[test]
    fn test_add_call_keyword_extend_list() {
        let source = "app = create_app(routers=[router], lifespans=[lifespan_a])\n";
        let result = add_call_keyword(source, "create_app", "lifespans", "lifespan_b").unwrap();
        assert!(result.contains("lifespan_a, lifespan_b"));
    }

    #[test]
    fn test_add_call_keyword_already_present() {
        let source = "app = create_app(lifespans=[lakebase_lifespan])\n";
        let err =
            add_call_keyword(source, "create_app", "lifespans", "lakebase_lifespan").unwrap_err();
        assert!(matches!(err, PyEditError::AlreadyPresent(_)));
    }

    #[test]
    fn test_add_call_keyword_call_not_found() {
        let source = "app = other_func()\n";
        let err =
            add_call_keyword(source, "create_app", "lifespans", "lakebase_lifespan").unwrap_err();
        assert!(matches!(err, PyEditError::NotFound(_)));
    }

    #[test]
    fn test_add_call_keyword_empty_args() {
        let source = "app = create_app()\n";
        let result =
            add_call_keyword(source, "create_app", "lifespans", "lakebase_lifespan").unwrap();
        assert!(result.contains("lifespans=[lakebase_lifespan]"));
    }

    #[test]
    fn test_add_import_idempotent() {
        let source = "import os\n";
        // First add should succeed
        let result = add_import(source, "import sys").unwrap();
        // Second add of same import should fail
        let err = add_import(&result, "import sys").unwrap_err();
        assert!(matches!(err, PyEditError::AlreadyPresent(_)));
    }

    #[test]
    fn test_add_class_member_with_alias_docstring() {
        let source = r#"class Dependencies:
    """Dependencies."""

    Client: TypeAlias = ClientDep
"#;
        let member = "Sql: TypeAlias = SqlDep\n\"\"\"SQL query dependency.\nRecommended usage: `sql: Dependencies.Sql`\"\"\"";
        let result = add_class_member(source, "Dependencies", member).unwrap();
        assert!(result.contains("    Sql: TypeAlias = SqlDep"));
        assert!(result.contains("    \"\"\"SQL query dependency."));
        assert!(result.contains("    Recommended usage: `sql: Dependencies.Sql`\"\"\""));

        // Idempotent: adding the same member again should return AlreadyPresent
        let err = add_class_member(&result, "Dependencies", member).unwrap_err();
        assert!(matches!(err, PyEditError::AlreadyPresent(_)));
    }

    #[test]
    fn test_add_class_member_type_alias() {
        let source = r#"class Dependencies:
    Client: TypeAlias = Annotated[WorkspaceClient, Depends(get_ws)]
    Config: TypeAlias = Annotated[AppConfig, Depends(get_config)]
"#;
        let result = add_class_member(
            source,
            "Dependencies",
            "Session: TypeAlias = Annotated[Session, Depends(get_session)]",
        )
        .unwrap();
        assert!(result.contains("Session: TypeAlias = Annotated[Session, Depends(get_session)]"));
    }
}
