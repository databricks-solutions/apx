use std::path::Path;
use tracing::trace;

/// Strip JSONC comments from input.
///
/// Supported:
/// - `// line comments`
/// - `/* block comments */`
///
/// Guarantees:
/// - Does NOT strip comment markers inside string literals
/// - Preserves newlines to keep line numbers stable
#[allow(dead_code)]
pub fn strip_jsonc_comments(input: &str) -> String {
    use tracing::debug;

    debug!(
        input_length = input.len(),
        "Starting JSONC comment stripping"
    );

    let mut out = String::with_capacity(input.len());

    let mut chars = input.chars().peekable();

    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut prev_was_escape = false;
    let mut line_num = 1;
    let mut col_num = 0;
    let mut chars_processed = 0;
    let mut comments_found = 0;
    let mut line_comments = 0;
    let mut block_comments = 0;

    while let Some(c) = chars.next() {
        chars_processed += 1;
        col_num += 1;

        if c == '\n' {
            line_num += 1;
            col_num = 0;
        }

        if in_line_comment {
            if c == '\n' {
                trace!(line = line_num, col = col_num, "Ending line comment");
                in_line_comment = false;
                out.push('\n');
            }
            continue;
        }

        if in_block_comment {
            if c == '*' && matches!(chars.peek(), Some('/')) {
                chars.next(); // consume '/'
                trace!(line = line_num, col = col_num, "Ending block comment");
                in_block_comment = false;
            } else if c == '\n' {
                // preserve newlines
                out.push('\n');
            }
            continue;
        }

        match c {
            '"' if !prev_was_escape => {
                in_string = !in_string;
                trace!(
                    line = line_num,
                    col = col_num,
                    in_string,
                    "String literal {}",
                    if in_string { "started" } else { "ended" }
                );
                out.push(c);
            }

            '/' if !in_string => match chars.peek() {
                Some('/') => {
                    chars.next();
                    line_comments += 1;
                    comments_found += 1;
                    trace!(line = line_num, col = col_num, "Found line comment (//)");
                    in_line_comment = true;
                }
                Some('*') => {
                    chars.next();
                    block_comments += 1;
                    comments_found += 1;
                    trace!(
                        line = line_num,
                        col = col_num,
                        "Found block comment start (/*)"
                    );
                    in_block_comment = true;
                }
                _ => {
                    trace!(
                        line = line_num,
                        col = col_num,
                        next_char = ?chars.peek(),
                        "Forward slash not part of comment, preserving"
                    );
                    out.push(c);
                }
            },

            '\\' if in_string => {
                trace!(line = line_num, col = col_num, "Escape character in string");
                out.push(c);
            }

            _ => out.push(c),
        }

        prev_was_escape = c == '\\' && !prev_was_escape;
    }

    debug!(
        output_length = out.len(),
        chars_processed,
        comments_found,
        line_comments,
        block_comments,
        reduction_bytes = input.len().saturating_sub(out.len()),
        "Comment stripping complete"
    );

    out
}

/// Format a path as relative to the app directory, with ./ prefix and cleaned up ././ patterns.
pub fn format_relative_path(path: &Path, app_dir: &Path) -> String {
    path.strip_prefix(app_dir)
        .map(format_relative_string)
        .unwrap_or_else(|_| path.display().to_string())
}

pub fn format_relative_string(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    // Remove leading ./ if present
    s.strip_prefix("./").unwrap_or(&s).to_string()
}
