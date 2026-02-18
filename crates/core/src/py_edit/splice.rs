//! Text insertion and indentation utilities.

/// Insert text at a byte offset in the source string.
pub fn insert_at(source: &str, offset: usize, text: &str) -> String {
    let mut result = String::with_capacity(source.len() + text.len());
    result.push_str(&source[..offset]);
    result.push_str(text);
    result.push_str(&source[offset..]);
    result
}

/// Indent a block of code by a given prefix (e.g. "    ").
pub fn indent_block(code: &str, indent: &str) -> String {
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
