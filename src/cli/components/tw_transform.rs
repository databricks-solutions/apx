//! Tailwind CSS v3 to v4 class syntax transformation utilities.
//!
//! This module handles the automatic transformation of Tailwind CSS v3 class syntax
//! to v4 syntax when adding components from registries that still use v3 format.

/// Transform Tailwind CSS v3 class syntax to v4 syntax.
///
/// Transformations:
/// - `[--custom-prop]` → `(--custom-prop)` for CSS custom properties in arbitrary values
/// - `has-[[data-attr=value]]` → `has-data-[attr=value]`  
/// - `group-has-[[data-attr=value]]` → `group-has-data-[attr=value]`
/// - `peer-has-[[data-attr=value]]` → `peer-has-data-[attr=value]`
/// - `!p-4` → `p-4!` (important modifier from prefix to suffix)
/// - `group-data-[x]:!p-4` → `group-data-[x]:p-4!`
pub fn transform_tailwind_v3_to_v4(content: &str) -> String {
    let mut result = content.to_string();
    
    // Transform CSS custom property syntax in arbitrary values: [--var] → (--var)
    // Matches patterns like w-[--sidebar-width], max-w-[--skeleton-width], etc.
    // But NOT data attributes like data-[state=open] or arbitrary values like w-[calc(...)]
    result = transform_css_var_syntax(&result);
    
    // Transform has-[[data-*]] → has-data-[*]
    // e.g., has-[[data-variant=inset]] → has-data-[variant=inset]
    result = result.replace("has-[[data-", "has-data-[");
    result = result.replace("group-has-[[data-", "group-has-data-[");
    result = result.replace("peer-has-[[data-", "peer-has-data-[");
    
    // Remove the extra closing bracket from the transformation above
    // has-data-[variant=inset]] → has-data-[variant=inset]
    result = fix_double_brackets(&result);
    
    // Transform important modifier from prefix to suffix
    // !p-4 → p-4!, hover:!bg-red → hover:bg-red!
    result = transform_important_modifier(&result);
    
    result
}

/// Transform CSS custom property syntax: [--var-name] → (--var-name)
/// Only transforms when the content starts with -- (CSS custom property)
fn transform_css_var_syntax(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    
    while let Some(c) = chars.next() {
        if c == '[' {
            // Check if this is a CSS custom property reference [--...]
            let mut bracket_content = String::new();
            let mut depth = 1;
            
            while let Some(&next) = chars.peek() {
                if next == '[' {
                    depth += 1;
                } else if next == ']' {
                    depth -= 1;
                    if depth == 0 {
                        chars.next(); // consume the closing ]
                        break;
                    }
                }
                bracket_content.push(chars.next().unwrap());
            }
            
            // If content starts with --, it's a CSS custom property - use parentheses
            if bracket_content.starts_with("--") && !bracket_content.contains('(') {
                result.push('(');
                result.push_str(&bracket_content);
                result.push(')');
            } else {
                // Keep original bracket syntax for other cases
                result.push('[');
                result.push_str(&bracket_content);
                result.push(']');
            }
        } else {
            result.push(c);
        }
    }
    
    result
}

/// Fix double closing brackets from has-data transformation
/// has-data-[variant=inset]] → has-data-[variant=inset]
fn fix_double_brackets(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    
    while let Some(c) = chars.next() {
        result.push(c);
        // If we just pushed a ] and the next char is also ], skip one
        if c == ']' {
            if let Some(&']') = chars.peek() {
                chars.next(); // skip the duplicate ]
            }
        }
    }
    
    result
}

/// Transform important modifier from prefix to suffix within className strings ONLY
/// Handles: "!p-4" → "p-4!", "hover:!bg-red" → "hover:bg-red!"
/// 
/// CRITICAL: Only transforms within double-quoted strings to avoid breaking JavaScript
/// negation operators like `!open` or `!isMobile`
fn transform_important_modifier(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = '"';
    
    while i < len {
        let c = chars[i];
        
        // Track string boundaries (handling escape sequences)
        if (c == '"' || c == '\'') && (i == 0 || chars[i - 1] != '\\') {
            if !in_string {
                in_string = true;
                string_char = c;
            } else if c == string_char {
                in_string = false;
            }
            result.push(c);
            i += 1;
            continue;
        }
        
        // Only transform !class patterns INSIDE strings
        if in_string && c == '!' && i > 0 {
            let prev = chars[i - 1];
            // Must be preceded by space, quote, or colon within string context
            if prev == ' ' || prev == '"' || prev == '\'' || prev == ':' {
                // Look ahead to capture the class name
                let mut class_name = String::new();
                let mut j = i + 1;
                
                // Capture valid Tailwind class characters
                while j < len {
                    let next = chars[j];
                    // Stop at string end or whitespace
                    if next == string_char || next == ' ' || next == '\n' || next == '\t' {
                        break;
                    }
                    if next.is_alphanumeric() || "-[]():/._%".contains(next) {
                        class_name.push(next);
                        j += 1;
                    } else {
                        break;
                    }
                }
                
                // Only transform if we captured a valid class name (starts with letter, contains hyphen = likely Tailwind)
                if !class_name.is_empty() 
                    && class_name.chars().next().unwrap().is_alphabetic()
                    && class_name.contains('-') 
                {
                    result.push_str(&class_name);
                    result.push('!');
                    i = j;
                    continue;
                }
            }
        }
        
        result.push(c);
        i += 1;
    }
    
    result
}