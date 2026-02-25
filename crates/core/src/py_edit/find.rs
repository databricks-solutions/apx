//! AST search utilities for finding imports, classes, and function calls.

use ruff_python_ast::{Expr, Keyword, Stmt, StmtClassDef};
use ruff_text_size::{Ranged, TextRange};

/// Find the byte-offset end of the last import statement.
/// Returns `None` if the file has no imports.
pub fn find_last_import_end(stmts: &[Stmt]) -> Option<usize> {
    let mut last_import_end: Option<usize> = None;
    for stmt in stmts {
        match stmt {
            Stmt::Import(_) | Stmt::ImportFrom(_) => {
                last_import_end = Some(stmt.range().end().into());
            }
            _ => {}
        }
    }
    last_import_end
}

/// Check if an exact import statement already exists in the source.
pub fn import_exists(source: &str, import_stmt: &str) -> bool {
    let trimmed = import_stmt.trim();
    source.lines().any(|line| line.trim() == trimmed)
}

/// Find a class definition by name. Returns the class def node's range and body info.
pub fn find_class<'a>(stmts: &'a [Stmt], class_name: &str) -> Option<&'a StmtClassDef> {
    for stmt in stmts {
        if let Stmt::ClassDef(class_def) = stmt
            && class_def.name.as_str() == class_name
        {
            return Some(class_def);
        }
    }
    None
}

/// Find the end offset of the last statement in a class body.
pub fn class_body_end(class_def: &StmtClassDef) -> usize {
    class_def.body.last().map_or_else(
        || class_def.range().end().into(),
        |stmt| stmt.range().end().into(),
    )
}

/// Detect the indentation level of the first statement in a class body.
pub fn class_body_indent(source: &str, class_def: &StmtClassDef) -> String {
    if let Some(first_stmt) = class_def.body.first() {
        let offset: usize = first_stmt.range().start().into();
        // Walk backwards from the statement start to find the line start
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let prefix = &source[line_start..offset];
        // Extract leading whitespace
        let indent: String = prefix.chars().take_while(|c| c.is_whitespace()).collect();
        return indent;
    }
    "    ".to_string() // default 4-space indent
}

/// Info about a found function call expression.
pub struct CallInfo {
    /// Existing keyword arguments.
    pub keywords: Vec<KeywordInfo>,
    /// The range of the arguments (inside the parentheses).
    pub args_end: usize,
}

pub struct KeywordInfo {
    pub name: String,
    /// If the keyword value is a list, the range of that list expression.
    pub list_range: Option<TextRange>,
}

/// Find a function call by the callee name (supports simple names and attribute access like `module.func`).
pub fn find_call(stmts: &[Stmt], call_target: &str) -> Option<CallInfo> {
    for stmt in stmts {
        if let Some(info) = find_call_in_stmt(stmt, call_target) {
            return Some(info);
        }
    }
    None
}

fn find_call_in_stmt(stmt: &Stmt, call_target: &str) -> Option<CallInfo> {
    // Check assignments: `app = create_app(...)`
    match stmt {
        Stmt::Assign(assign) => find_call_in_expr(&assign.value, call_target),
        Stmt::AnnAssign(ann_assign) => ann_assign
            .value
            .as_ref()
            .and_then(|v| find_call_in_expr(v, call_target)),
        Stmt::Expr(expr_stmt) => find_call_in_expr(&expr_stmt.value, call_target),
        _ => None,
    }
}

fn find_call_in_expr(expr: &Expr, call_target: &str) -> Option<CallInfo> {
    if let Expr::Call(call) = expr {
        let matches = match call.func.as_ref() {
            Expr::Name(name) => name.id.as_str() == call_target,
            Expr::Attribute(attr) => {
                let full = format!("{}.{}", expr_name(&attr.value), attr.attr);
                full == call_target
            }
            _ => false,
        };

        if matches {
            let keywords: Vec<KeywordInfo> = call
                .arguments
                .keywords
                .iter()
                .filter_map(keyword_info)
                .collect();

            return Some(CallInfo {
                keywords,
                args_end: usize::from(call.range.end()) - 1, // position of closing `)`
            });
        }
    }
    None
}

fn keyword_info(kw: &Keyword) -> Option<KeywordInfo> {
    let name = kw.arg.as_ref()?.to_string();
    let list_range = if let Expr::List(list) = &kw.value {
        Some(list.range)
    } else {
        None
    };
    Some(KeywordInfo { name, list_range })
}

fn expr_name(expr: &Expr) -> String {
    match expr {
        Expr::Name(name) => name.id.to_string(),
        Expr::Attribute(attr) => format!("{}.{}", expr_name(&attr.value), attr.attr),
        _ => String::new(),
    }
}
