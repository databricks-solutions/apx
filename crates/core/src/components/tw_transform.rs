//! Tailwind CSS v3 to v4 class syntax transformation utilities.
//!
//! This module handles the automatic transformation of Tailwind CSS v3 class syntax
//! to v4 syntax when adding components from registries that still use v3 format.
//!
//! Architecture: source is tokenized into Code/StringContent regions, then only
//! StringContent regions have per-token class transforms applied. Each transform
//! is a pure `fn(&str) -> Cow<str>` operating on a single whitespace-delimited token.

use std::borrow::Cow;

// ─── Region types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionKind {
    Code,
    StringContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region<'a> {
    text: &'a str,
    kind: RegionKind,
}

/// A class-level transform: takes a single whitespace-delimited token,
/// returns `Cow::Borrowed` if unchanged, `Cow::Owned` if transformed.
type ClassTransformFn = for<'a> fn(&'a str) -> Cow<'a, str>;

/// Ordered list of per-token transforms applied to every class token inside strings.
const CLASS_TRANSFORMS: &[ClassTransformFn] =
    &[css_var_syntax, has_data_shorthand, important_modifier];

// ─── Region tokenizer ────────────────────────────────────────────────────────

/// Split source into alternating Code / StringContent regions.
///
/// Recognizes `"`, `'`, and `` ` `` as string delimiters (handling `\` escapes).
/// The delimiter characters themselves belong to Code regions so that transforms
/// never see (or corrupt) quotes.
fn tokenize_regions(input: &str) -> Vec<Region<'_>> {
    let mut regions = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    // Start of the current region being accumulated
    let mut region_start = 0;

    while i < len {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            // Emit any Code region accumulated before this quote
            if i > region_start {
                regions.push(Region {
                    text: &input[region_start..i],
                    kind: RegionKind::Code,
                });
            }
            // The opening quote itself is Code
            regions.push(Region {
                text: &input[i..i + 1],
                kind: RegionKind::Code,
            });
            i += 1; // move past opening quote

            let string_start = i;
            // Scan for closing quote
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2; // skip escaped char
                    continue;
                }
                if bytes[i] == quote {
                    break;
                }
                i += 1;
            }
            // Emit the string content (possibly empty)
            if i > string_start {
                regions.push(Region {
                    text: &input[string_start..i],
                    kind: RegionKind::StringContent,
                });
            }
            // Emit the closing quote as Code (if we found one)
            if i < len {
                regions.push(Region {
                    text: &input[i..i + 1],
                    kind: RegionKind::Code,
                });
                i += 1;
            }
            region_start = i;
        } else {
            i += 1;
        }
    }
    // Emit trailing Code region
    if region_start < len {
        regions.push(Region {
            text: &input[region_start..len],
            kind: RegionKind::Code,
        });
    }
    regions
}

// ─── Pipeline orchestrator ───────────────────────────────────────────────────

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
    let regions = tokenize_regions(content);
    let mut out = String::with_capacity(content.len());
    for region in &regions {
        match region.kind {
            RegionKind::Code => out.push_str(region.text),
            RegionKind::StringContent => {
                transform_string_classes(region.text, &mut out);
            }
        }
    }
    out
}

/// Iterate whitespace-separated tokens in a string region, applying class
/// transforms to each token while preserving the original whitespace.
fn transform_string_classes(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Accumulate whitespace
        let ws_start = i;
        while i < len
            && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r')
        {
            i += 1;
        }
        if i > ws_start {
            out.push_str(&s[ws_start..i]);
        }
        if i >= len {
            break;
        }
        // Accumulate non-whitespace token
        let tok_start = i;
        while i < len
            && bytes[i] != b' '
            && bytes[i] != b'\t'
            && bytes[i] != b'\n'
            && bytes[i] != b'\r'
        {
            i += 1;
        }
        let token = &s[tok_start..i];
        let transformed = transform_class_token(token);
        out.push_str(&transformed);
    }
}

/// Apply all `CLASS_TRANSFORMS` to a single class token, chaining results.
fn transform_class_token(token: &str) -> Cow<'_, str> {
    let mut current: Cow<'_, str> = Cow::Borrowed(token);
    for transform in CLASS_TRANSFORMS {
        match current {
            Cow::Borrowed(s) => {
                current = transform(s);
            }
            Cow::Owned(ref s) => {
                let result = transform(s.as_str());
                if let Cow::Owned(new) = result {
                    current = Cow::Owned(new);
                }
                // If Borrowed, it borrowed from the Owned string — keep current as-is
            }
        }
    }
    current
}

// ─── Per-token transforms ────────────────────────────────────────────────────

/// Transform CSS custom property syntax: `[--var-name]` → `(--var-name)`
///
/// Only transforms when bracket content starts with `--` (CSS custom property)
/// and doesn't contain `(` (not a calc expression).
fn css_var_syntax(token: &str) -> Cow<'_, str> {
    // Find `[--` in the token
    let Some(open) = token.find("[--") else {
        return Cow::Borrowed(token);
    };

    // Track bracket depth to find the matching `]`
    let bytes = token.as_bytes();
    let mut depth = 0;
    let mut close = None;
    for (j, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
    }

    let Some(close) = close else {
        // Unclosed bracket — return as-is
        return Cow::Borrowed(token);
    };

    let bracket_content = &token[open + 1..close]; // content between [ and ]
    // Only transform if it starts with -- and doesn't contain ( (calc)
    if bracket_content.starts_with("--") && !bracket_content.contains('(') {
        let mut result = String::with_capacity(token.len());
        result.push_str(&token[..open]);
        result.push('(');
        result.push_str(bracket_content);
        result.push(')');
        result.push_str(&token[close + 1..]);
        Cow::Owned(result)
    } else {
        Cow::Borrowed(token)
    }
}

/// Transform v3 data shorthand: `has-[[data-attr=val]]` → `has-data-[attr=val]`
///
/// Also handles `group-has-` and `peer-has-` prefixes.
/// Does NOT transform complex CSS selectors inside `has-[...]`.
fn has_data_shorthand(token: &str) -> Cow<'_, str> {
    // Look for any of the prefix patterns in the token
    for prefix in ["has-", "group-has-", "peer-has-"] {
        let search = format!("{prefix}[[data-");
        if let Some(pos) = token.find(&search) {
            let after = &token[pos + search.len()..];
            if let Some(end) = find_simple_data_attr_end(after) {
                let attr_content = &after[..end];
                let mut result = String::with_capacity(token.len());
                result.push_str(&token[..pos]);
                result.push_str(prefix);
                result.push_str("data-[");
                result.push_str(attr_content);
                result.push(']');
                result.push_str(&after[end + 2..]); // skip past ]]
                return Cow::Owned(result);
            }
        }
    }
    Cow::Borrowed(token)
}

/// Transform important modifier from prefix to suffix.
///
/// - `!class-name` → `class-name!`
/// - `variant:!class-name` → `variant:class-name!`
fn important_modifier(token: &str) -> Cow<'_, str> {
    if let Some(class) = token.strip_prefix('!') {
        // Token form: !class-name → class-name!
        if is_likely_tailwind_class(class) {
            let mut result = String::with_capacity(token.len());
            result.push_str(class);
            result.push('!');
            return Cow::Owned(result);
        }
    } else if let Some(colon_pos) = rfind_variant_colon(token) {
        // Check for variant:!class pattern
        let after_colon = &token[colon_pos + 1..];
        if let Some(class) = after_colon
            .strip_prefix('!')
            .filter(|c| is_likely_tailwind_class(c))
        {
            let mut result = String::with_capacity(token.len());
            result.push_str(&token[..colon_pos + 1]); // variant:
            result.push_str(class);
            result.push('!');
            return Cow::Owned(result);
        }
    }
    Cow::Borrowed(token)
}

/// Check if a string looks like a Tailwind class name.
/// Must start with a letter and contain a hyphen.
fn is_likely_tailwind_class(s: &str) -> bool {
    !s.is_empty() && s.as_bytes()[0].is_ascii_alphabetic() && s.contains('-')
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Check if the text after `has-[[data-` is a simple attribute value ending with `]]`.
///
/// Returns `Some(end_offset)` pointing to the first `]` of the closing `]]` if the
/// content is a simple `WORD` or `WORD=VALUE` (no brackets, colons, spaces, or other
/// selector syntax). Returns `None` if it's a complex selector.
fn find_simple_data_attr_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b']' => {
                // Must be followed by another ] to form ]]
                if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                    // Verify we consumed at least something (not empty)
                    if i == 0 {
                        return None;
                    }
                    return Some(i);
                }
                // Single ] means there's more complex selector content
                return None;
            }
            // These characters indicate a complex selector, not simple shorthand
            b'[' | b':' | b' ' | b'>' | b'+' | b'~' | b'(' | b')' => return None,
            _ => continue,
        }
    }
    None // No closing ]] found
}

/// Bracket-aware right-to-left scan for the last variant colon separator.
///
/// Returns `Some(byte_offset)` of the rightmost `:` that is at bracket depth 0,
/// i.e. a true Tailwind variant separator (not inside `[...]` or `(...)`).
fn rfind_variant_colon(token: &str) -> Option<usize> {
    let bytes = token.as_bytes();
    let mut depth: i32 = 0;
    let mut last_colon = None;

    // Scan right-to-left
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b']' | b')' => depth += 1,
            b'[' | b'(' => depth -= 1,
            b':' if depth == 0 => {
                last_colon = Some(i);
                // We want the rightmost, so return immediately
                return last_colon;
            }
            _ => {}
        }
    }
    last_colon
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Bug #114 — input-group.tsx broken selectors
    // These tests reproduce the exact patterns from the issue report.
    // =========================================================================

    #[test]
    fn test_issue_114_has_child_data_align_preserved() {
        // has-[>[data-align=block-end]] is NOT a v3 data shorthand — it's a :has(>)
        // child combinator selector. The ]] is legitimate nested brackets.
        let input =
            r#""has-[>[data-align=block-end]]:h-auto has-[>[data-align=block-end]]:flex-col""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "has-[>[data-align=...]] must be preserved");
    }

    #[test]
    fn test_issue_114_all_alignment_variants() {
        // All 4 alignment selectors from input-group.tsx
        let cases = [
            r#""has-[>[data-align=inline-start]]:[&>input]:pl-2""#,
            r#""has-[>[data-align=inline-end]]:[&>input]:pr-2""#,
            r#""has-[>[data-align=block-start]]:h-auto has-[>[data-align=block-start]]:flex-col has-[>[data-align=block-start]]:[&>input]:pb-3""#,
            r#""has-[>[data-align=block-end]]:h-auto has-[>[data-align=block-end]]:flex-col has-[>[data-align=block-end]]:[&>input]:pt-3""#,
        ];
        for input in cases {
            let result = transform_tailwind_v3_to_v4(input);
            assert_eq!(
                result, input,
                "alignment selector must be preserved: {input}"
            );
        }
    }

    #[test]
    fn test_issue_114_compound_attribute_selectors_preserved() {
        // has-[[data-slot][aria-invalid=true]] — compound attribute selectors, NOT v3 shorthand
        let input = r#""has-[[data-slot][aria-invalid=true]]:ring-destructive/20 has-[[data-slot][aria-invalid=true]]:border-destructive dark:has-[[data-slot][aria-invalid=true]]:ring-destructive/40""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(
            result, input,
            "compound attribute selectors inside has-[] must be preserved"
        );
    }

    #[test]
    fn test_issue_114_pseudo_class_in_has_preserved() {
        // has-[[data-slot=input-group-control]:focus-visible] — attribute + pseudo-class
        let input = r#""has-[[data-slot=input-group-control]:focus-visible]:ring-ring has-[[data-slot=input-group-control]:focus-visible]:ring-1""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(
            result, input,
            "attribute selector with pseudo-class in has-[] must be preserved"
        );
    }

    // =========================================================================
    // V3→V4 data shorthand transforms that SHOULD work
    // These are the legitimate transforms the function is designed for.
    // =========================================================================

    #[test]
    fn test_simple_has_data_shorthand() {
        // has-[[data-variant=inset]] → has-data-[variant=inset]
        // This is the canonical v3 shorthand pattern from sidebar.tsx
        let input = r#""has-[[data-variant=inset]]:bg-sidebar""#;
        let expected = r#""has-data-[variant=inset]:bg-sidebar""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_group_has_data_shorthand() {
        // group-has-[[data-sidebar=menu-action]] from sidebar.tsx
        let input = r#""group-has-[[data-sidebar=menu-action]]/menu-item:pr-8""#;
        let expected = r#""group-has-data-[sidebar=menu-action]/menu-item:pr-8""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_group_has_data_collapsible() {
        // group-has-[[data-collapsible=icon]] from sidebar-13.json
        let input = r#""group-has-[[data-collapsible=icon]]/sidebar-wrapper:h-12""#;
        let expected = r#""group-has-data-[collapsible=icon]/sidebar-wrapper:h-12""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_group_has_data_slot_item_description() {
        // group-has-[[data-slot=item-description]] from item.json
        let input = r#""group-has-[[data-slot=item-description]]/item:translate-y-0.5 group-has-[[data-slot=item-description]]/item:self-start""#;
        let expected = r#""group-has-data-[slot=item-description]/item:translate-y-0.5 group-has-data-[slot=item-description]/item:self-start""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_group_has_data_orientation() {
        // group-has-[[data-orientation=horizontal]] from field.json
        let input = r#""group-has-[[data-orientation=horizontal]]/field:text-balance""#;
        let expected = r#""group-has-data-[orientation=horizontal]/field:text-balance""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_has_data_no_value() {
        // has-[[data-slot]] (attribute without value) — still simple shorthand
        let input = r#""has-[[data-active]]:bg-accent""#;
        let expected = r#""has-data-[active]:bg-accent""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    // =========================================================================
    // Patterns from real components that must be PRESERVED (not transformed)
    // =========================================================================

    #[test]
    fn test_has_child_combinator_preserved() {
        // has-[>textarea], has-[>button], has-[>svg] from input-group.tsx
        let cases = [
            r#""has-[>textarea]:h-auto""#,
            r#""has-[>button]:ml-[-0.45rem]""#,
            r#""has-[>svg]:px-2""#,
            r#""has-[>input]/input-group:pt-2.5""#,
        ];
        for input in cases {
            let result = transform_tailwind_v3_to_v4(input);
            assert_eq!(result, input, "simple has-[>element] must be preserved");
        }
    }

    #[test]
    fn test_has_element_attribute_preserved() {
        // has-[select[aria-hidden=true]:last-child] from button-group.json
        let input = r#""has-[select[aria-hidden=true]:last-child]:[&>[data-slot=select-trigger]:last-of-type]:rounded-r-md""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(
            result, input,
            "has-[element[attr]:pseudo] must be preserved"
        );
    }

    #[test]
    fn test_has_child_data_slot_field() {
        // has-[>[data-slot=checkbox-group]] from field.json
        let cases = [
            r#""has-[>[data-slot=checkbox-group]]:gap-3 has-[>[data-slot=radio-group]]:gap-3""#,
            r#""has-[>[data-slot=field-content]]:[&>[role=checkbox],[role=radio]]:mt-px has-[>[data-slot=field-content]]:items-start""#,
            r#""has-[>[data-slot=field]]:w-full has-[>[data-slot=field]]:flex-col""#,
            r#""has-[>[data-slot=button-group]]:gap-2""#,
        ];
        for input in cases {
            let result = transform_tailwind_v3_to_v4(input);
            assert_eq!(result, input, "has-[>[data-slot=...]] must be preserved");
        }
    }

    #[test]
    fn test_has_disabled_preserved() {
        // has-[:disabled] from input-otp.json
        let input = r#""has-[:disabled]:opacity-50""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_ancestor_selector_brackets_preserved() {
        // [[data-side=left]_&] from sidebar.tsx — ancestor selector syntax
        let cases = [
            r#""[[data-side=left]_&]:cursor-w-resize [[data-side=right]_&]:cursor-e-resize""#,
            r#""[[data-side=left][data-state=collapsed]_&]:cursor-e-resize""#,
            r#""[[data-side=left][data-collapsible=offcanvas]_&]:-right-2""#,
        ];
        for input in cases {
            let result = transform_tailwind_v3_to_v4(input);
            assert_eq!(result, input, "ancestor selector [[...]] must be preserved");
        }
    }

    #[test]
    fn test_group_has_simple_element_preserved() {
        // group-has-[>input] from input-group.tsx — no data shorthand
        let input = r#""group-has-[>input]/input-group:pt-2.5""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    // =========================================================================
    // CSS custom property transform tests
    // =========================================================================

    #[test]
    fn test_css_var_simple() {
        let input = r#""w-[--sidebar-width]""#;
        let expected = r#""w-(--sidebar-width)""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_css_var_in_skeleton() {
        // max-w-[--skeleton-width] from sidebar.tsx
        let input = r#""h-4 max-w-[--skeleton-width] flex-1""#;
        let expected = r#""h-4 max-w-(--skeleton-width) flex-1""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_css_var_calc_not_transformed() {
        // calc() references should NOT be transformed
        let input = r#""w-[calc(var(--sidebar-width)*-1)]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "calc() must not be changed to parentheses");
    }

    #[test]
    fn test_data_attr_not_transformed_as_var() {
        // data-[state=open] is NOT a CSS custom property
        let input = r#""data-[state=open]:bg-accent""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_css_var_double_dash_cell_size() {
        // [--cell-size:2.5rem] from calendar components
        let input = r#""bg-transparent p-0 [--cell-size:2.5rem] md:[--cell-size:3rem]""#;
        let expected = r#""bg-transparent p-0 (--cell-size:2.5rem) md:(--cell-size:3rem)""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_css_var_color_border() {
        // border-[--color-border] from chart components
        let input = r#""border-[--color-border] bg-[--color-bg]""#;
        let expected = r#""border-(--color-border) bg-(--color-bg)""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    // =========================================================================
    // Important modifier transform tests
    // =========================================================================

    #[test]
    fn test_important_prefix_to_suffix() {
        let input = r#""group-data-[collapsible=icon]:!size-8 group-data-[collapsible=icon]:!p-2""#;
        let expected =
            r#""group-data-[collapsible=icon]:size-8! group-data-[collapsible=icon]:p-2!""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_important_js_negation_not_transformed() {
        // !open and !isMobile are JS negations, not Tailwind important
        let input = r#"if (!open && !isMobile) { return }"#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_important_in_sidebar_lg() {
        // "h-12 text-sm group-data-[collapsible=icon]:!p-0"
        let input = r#""h-12 text-sm group-data-[collapsible=icon]:!p-0""#;
        let expected = r#""h-12 text-sm group-data-[collapsible=icon]:p-0!""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_important_with_bang_m() {
        // "!m-0" from button-group.json
        let input = r#""relative !m-0 self-stretch""#;
        let expected = r#""relative m-0! self-stretch""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    // =========================================================================
    // Full component integration tests — real file contents
    // =========================================================================

    #[test]
    fn test_full_input_group_classname() {
        // The exact className block from input-group.tsx (from registry JSON)
        let input = concat!(
            r#""group/input-group border-input dark:bg-input/30 shadow-xs relative flex w-full items-center rounded-md border outline-none transition-[color,box-shadow]","#,
            "\n",
            r#""h-9 has-[>textarea]:h-auto","#,
            "\n",
            r#""has-[>[data-align=inline-start]]:[&>input]:pl-2","#,
            "\n",
            r#""has-[>[data-align=inline-end]]:[&>input]:pr-2","#,
            "\n",
            r#""has-[>[data-align=block-start]]:h-auto has-[>[data-align=block-start]]:flex-col has-[>[data-align=block-start]]:[&>input]:pb-3","#,
            "\n",
            r#""has-[>[data-align=block-end]]:h-auto has-[>[data-align=block-end]]:flex-col has-[>[data-align=block-end]]:[&>input]:pt-3","#,
            "\n",
            r#""has-[[data-slot=input-group-control]:focus-visible]:ring-ring has-[[data-slot=input-group-control]:focus-visible]:ring-1","#,
            "\n",
            r#""has-[[data-slot][aria-invalid=true]]:ring-destructive/20 has-[[data-slot][aria-invalid=true]]:border-destructive dark:has-[[data-slot][aria-invalid=true]]:ring-destructive/40""#,
        );
        let result = transform_tailwind_v3_to_v4(input);
        // NONE of these patterns should be changed — they're all v4 CSS selectors
        assert_eq!(
            result, input,
            "input-group.tsx classNames must be preserved exactly"
        );
    }

    #[test]
    fn test_full_sidebar_menu_button() {
        // The sidebarMenuButtonVariants string from sidebar.tsx — contains both
        // a v3 shorthand (group-has-[[data-sidebar=menu-action]]) AND important modifiers
        let input = r#""peer/menu-button flex w-full items-center gap-2 overflow-hidden rounded-md p-2 text-left text-sm outline-none ring-sidebar-ring transition-[width,height,padding] hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 active:bg-sidebar-accent active:text-sidebar-accent-foreground disabled:pointer-events-none disabled:opacity-50 group-has-[[data-sidebar=menu-action]]/menu-item:pr-8 aria-disabled:pointer-events-none aria-disabled:opacity-50 data-[active=true]:bg-sidebar-accent data-[active=true]:font-medium data-[active=true]:text-sidebar-accent-foreground data-[state=open]:hover:bg-sidebar-accent data-[state=open]:hover:text-sidebar-accent-foreground group-data-[collapsible=icon]:!size-8 group-data-[collapsible=icon]:!p-2 [&>span:last-child]:truncate [&>svg]:size-4 [&>svg]:shrink-0""#;

        let result = transform_tailwind_v3_to_v4(input);

        // v3 shorthand should be transformed
        assert!(
            result.contains("group-has-data-[sidebar=menu-action]/menu-item:pr-8"),
            "v3 data shorthand should be transformed"
        );
        // Important modifiers should be transformed
        assert!(
            result.contains("group-data-[collapsible=icon]:size-8!"),
            "!size-8 should become size-8!"
        );
        assert!(
            result.contains("group-data-[collapsible=icon]:p-2!"),
            "!p-2 should become p-2!"
        );
        // data-[...] attributes must be preserved
        assert!(result.contains("data-[active=true]:bg-sidebar-accent"));
        assert!(result.contains("data-[state=open]:hover:bg-sidebar-accent"));
    }

    #[test]
    fn test_full_sidebar_provider() {
        // SidebarProvider className with has-[[data-variant=inset]] — simple shorthand
        let input = r#""group/sidebar-wrapper flex min-h-svh w-full has-[[data-variant=inset]]:bg-sidebar""#;
        let expected =
            r#""group/sidebar-wrapper flex min-h-svh w-full has-data-[variant=inset]:bg-sidebar""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_full_sidebar_rail() {
        // SidebarRail with ancestor selectors [[data-side=left]_&] — must preserve ]]
        let input =
            r#""[[data-side=left]_&]:cursor-w-resize [[data-side=right]_&]:cursor-e-resize""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "ancestor selector ]] must be preserved");
    }

    #[test]
    fn test_full_sidebar_rail_compound_ancestor() {
        let input = r#""[[data-side=left][data-state=collapsed]_&]:cursor-e-resize [[data-side=right][data-state=collapsed]_&]:cursor-w-resize""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(
            result, input,
            "compound ancestor selector ]] must be preserved"
        );
    }

    #[test]
    fn test_full_sidebar_skeleton_css_var() {
        // Skeleton with max-w-[--skeleton-width] — CSS var transform
        let input = r#""h-4 max-w-[--skeleton-width] flex-1""#;
        let expected = r#""h-4 max-w-(--skeleton-width) flex-1""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_full_sidebar_gap_css_var() {
        // group-data-[collapsible=icon]:w-[--sidebar-width-icon] — CSS var transform
        let input = r#""group-data-[collapsible=icon]:w-[--sidebar-width-icon]""#;
        let expected = r#""group-data-[collapsible=icon]:w-(--sidebar-width-icon)""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_full_field_description() {
        // FieldDescription from field.json — group-has-[[data-orientation=horizontal]]
        let input = r#""text-muted-foreground text-sm font-normal leading-normal group-has-[[data-orientation=horizontal]]/field:text-balance""#;
        let expected = r#""text-muted-foreground text-sm font-normal leading-normal group-has-data-[orientation=horizontal]/field:text-balance""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_full_field_label_mixed() {
        // FieldLabel from field.json — has v4 has-data-[state=checked] (already v4) and
        // has-[>[data-slot=field]] (must preserve)
        let input = r#""has-[>[data-slot=field]]:w-full has-[>[data-slot=field]]:flex-col has-[>[data-slot=field]]:rounded-md has-[>[data-slot=field]]:border [&>[data-slot=field]]:p-4""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "has-[>[data-slot=...]] must be preserved");
    }

    #[test]
    fn test_full_item_media() {
        // ItemMedia from item.json — group-has-[[data-slot=item-description]]
        let input = r#""flex shrink-0 items-center justify-center gap-2 group-has-[[data-slot=item-description]]/item:translate-y-0.5 group-has-[[data-slot=item-description]]/item:self-start [&_svg]:pointer-events-none""#;
        let expected = r#""flex shrink-0 items-center justify-center gap-2 group-has-data-[slot=item-description]/item:translate-y-0.5 group-has-data-[slot=item-description]/item:self-start [&_svg]:pointer-events-none""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_full_button_group() {
        // ButtonGroup from button-group.json — has both has-[>[data-slot=button-group]]
        // and has-[select[aria-hidden=true]:last-child] — neither should transform
        let input = r#""flex w-fit items-stretch has-[>[data-slot=button-group]]:gap-2 [&>*]:focus-visible:relative [&>*]:focus-visible:z-10 has-[select[aria-hidden=true]:last-child]:[&>[data-slot=select-trigger]:last-of-type]:rounded-r-md""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "button-group selectors must be preserved");
    }

    #[test]
    fn test_full_field_responsive_container() {
        // Responsive container query with has-[>[data-slot=field-content]] from field.json
        let input = r#""@md/field-group:has-[>[data-slot=field-content]]:items-start @md/field-group:has-[>[data-slot=field-content]]:[&>[role=checkbox],[role=radio]]:mt-px""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "container query has-[] must be preserved");
    }

    // =========================================================================
    // Edge cases and regression tests
    // =========================================================================

    #[test]
    fn test_already_v4_has_data_not_double_transformed() {
        // If content already has v4 syntax, don't break it
        let input = r#""has-data-[state=checked]:bg-primary/5""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input, "already-v4 has-data-[] must be preserved");
    }

    #[test]
    fn test_multiple_transforms_in_one_line() {
        // Mix of v3 shorthand + css var + important in one string
        let input = r#""has-[[data-variant=inset]]:bg-sidebar w-[--sidebar-width] group-data-[collapsible=icon]:!p-2""#;
        let expected = r#""has-data-[variant=inset]:bg-sidebar w-(--sidebar-width) group-data-[collapsible=icon]:p-2!""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_empty_content() {
        let result = transform_tailwind_v3_to_v4("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_no_transforms_needed() {
        let input = r#"const x = "flex items-center gap-2 text-sm";"#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_peer_has_data_shorthand() {
        // peer-has- variant (not seen in real data but supported)
        let input = r#""peer-has-[[data-active=true]]:bg-accent""#;
        let expected = r#""peer-has-data-[active=true]:bg-accent""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_stroke_dasharray_arbitrary_value() {
        // [&_[stroke-dasharray='1px_1px']] from animate-ui — complex arbitrary selector
        // This is inside single-quoted strings within the outer double-quoted string.
        // The region tokenizer splits on quotes, so the inner quotes create
        // separate regions. The important thing is no panic and ]] is preserved.
        let input = r#"[&_[stroke-dasharray='1px_1px']]:![stroke-dasharray:1px_0px]"#;
        let result = transform_tailwind_v3_to_v4(input);
        // This is all Code (no quotes wrapping it), so it passes through unchanged
        assert_eq!(
            result, input,
            "code outside strings must pass through unchanged"
        );
    }

    // =========================================================================
    // Unit tests for find_simple_data_attr_end
    // =========================================================================

    #[test]
    fn test_find_simple_attr_with_value() {
        // "variant=inset]]..." → Some(13) pointing to first ]
        assert_eq!(find_simple_data_attr_end("variant=inset]]"), Some(13));
    }

    #[test]
    fn test_find_simple_attr_no_value() {
        // "active]]" → Some(6)
        assert_eq!(find_simple_data_attr_end("active]]"), Some(6));
    }

    #[test]
    fn test_find_complex_attr_with_pseudo() {
        // "slot=input-group-control]:focus-visible]" — has single ] then more content
        assert_eq!(
            find_simple_data_attr_end("slot=input-group-control]:focus-visible]"),
            None
        );
    }

    #[test]
    fn test_find_compound_attrs() {
        // "slot][aria-invalid=true]]" — has inner [ which is complex
        assert_eq!(find_simple_data_attr_end("slot][aria-invalid=true]]"), None);
    }

    #[test]
    fn test_find_empty() {
        // "]]" → None (empty attribute name)
        assert_eq!(find_simple_data_attr_end("]]"), None);
    }

    #[test]
    fn test_find_no_closing() {
        // "variant=inset" → None (no ]])
        assert_eq!(find_simple_data_attr_end("variant=inset"), None);
    }

    #[test]
    fn test_find_single_bracket_only() {
        // "variant=inset]" → None (only single ])
        assert_eq!(find_simple_data_attr_end("variant=inset]"), None);
    }

    // =========================================================================
    // Corner cases: malformed input, truncation, prefix collisions
    // =========================================================================

    #[test]
    fn test_has_data_truncated_no_closing_brackets() {
        // Malformed: has-[[data-variant=inset without closing ]]
        // With region tokenizer, the string content is processed per-token.
        // The token has-[[data-variant=inset has no ]] so find_simple_data_attr_end returns None.
        let input = r#""has-[[data-variant=inset""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert!(!result.is_empty(), "truncated input must not panic");
    }

    #[test]
    fn test_has_data_truncated_single_bracket() {
        // Malformed: has-[[data-variant=inset] — only one closing bracket
        let input = r#""has-[[data-variant=inset]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert!(!result.is_empty(), "single ] must not panic");
    }

    #[test]
    fn test_has_data_empty_attr_name() {
        // has-[[data-]] — empty attribute name after data-
        let input = r#""has-[[data-]]:bg-red""#;
        let result = transform_tailwind_v3_to_v4(input);
        // find_simple_data_attr_end sees "]" at position 0 → checks for ]] → i=0 guard returns None
        // So this falls through as non-simple. The original text is preserved.
        assert_eq!(result, input, "empty attr name must not transform");
    }

    #[test]
    fn test_has_data_at_end_of_string() {
        // Pattern at very end of input, ]] are the last characters
        // Note: outside of quotes, this is Code, so it won't be transformed.
        // Wrap in quotes so it's a string.
        let input = r#""has-[[data-foo]]""#;
        let expected = r#""has-data-[foo]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected, "pattern at end of string must work");
    }

    #[test]
    fn test_has_data_only_pattern() {
        // Input is ONLY the pattern inside quotes
        let input = r#""has-[[data-x=1]]""#;
        let expected = r#""has-data-[x=1]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_has_data_prefix_collision_with_unrelated_word() {
        // "foobarbaz-has-[[data-x]]" — "has-" appears inside a non-Tailwind word.
        // The function transforms it because it can't distinguish context.
        // This is acceptable: such patterns don't occur in real Tailwind/JSX.
        let input = r#""foobarbaz-has-[[data-x=1]]:bg-red""#;
        let result = transform_tailwind_v3_to_v4(input);
        // We document this as a known limitation rather than a bug
        assert!(
            result.contains("has-data-[x=1]"),
            "substring match transforms (acceptable: doesn't occur in real code)"
        );
    }

    #[test]
    fn test_has_data_multiple_consecutive() {
        // Two v3 shorthands back-to-back
        let input = r#""has-[[data-a=1]]:x has-[[data-b=2]]:y""#;
        let expected = r#""has-data-[a=1]:x has-data-[b=2]:y""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_has_data_adjacent_to_non_simple() {
        // Mix of simple shorthand and complex selector on the same line
        let input = r#""has-[[data-variant=inset]]:bg-red has-[[data-slot]:focus-visible]:ring-1""#;
        let expected =
            r#""has-data-[variant=inset]:bg-red has-[[data-slot]:focus-visible]:ring-1""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(
            result, expected,
            "simple shorthand transforms, complex selector preserved"
        );
    }

    #[test]
    fn test_has_data_group_and_plain_in_same_line() {
        // Both has- and group-has- with simple shorthands
        let input = r#""has-[[data-a=1]]:x group-has-[[data-b=2]]:y peer-has-[[data-c=3]]:z""#;
        let expected = r#""has-data-[a=1]:x group-has-data-[b=2]:y peer-has-data-[c=3]:z""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_find_attr_with_hyphenated_value() {
        // Attribute value containing hyphens (common in data attributes)
        // "slot=item-description" is 21 chars (0..=20), ] is at index 21
        assert_eq!(
            find_simple_data_attr_end("slot=item-description]]"),
            Some(21)
        );
    }

    #[test]
    fn test_find_attr_with_dots_and_numbers() {
        // Values can contain dots, numbers, etc.
        assert_eq!(find_simple_data_attr_end("size=1.5]]"), Some(8));
    }

    #[test]
    fn test_has_data_triple_bracket() {
        // Pathological: has-[[data-x]]] — three closing brackets
        // As a token, has_data_shorthand finds the first ]] and transforms
        let input = r#""has-[[data-x]]]extra""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert!(
            result.contains("has-data-[x]"),
            "triple bracket transforms the first ]] pair"
        );
    }

    #[test]
    fn test_has_data_input_is_just_prefix() {
        // Input ends immediately after the search prefix (inside a string)
        let input = r#""has-[[data-""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert!(!result.is_empty(), "truncated at prefix must not panic");
    }

    #[test]
    fn test_css_var_unclosed_bracket() {
        // Malformed: [--foo without closing ]
        let input = r#""w-[--foo""#;
        let result = transform_tailwind_v3_to_v4(input);
        // With per-token transform, the token is "w-[--foo" (no closing bracket)
        // css_var_syntax finds [-- but no matching ] → returns Borrowed
        // So the token passes through unchanged
        assert_eq!(result, input, "unclosed bracket passes through unchanged");
    }

    #[test]
    fn test_has_data_with_newlines_in_content() {
        // Pattern split across lines (unlikely but possible in template literals)
        // With whitespace-based tokenization, newline splits the token so this
        // pattern wouldn't match as a single token anyway
        let input = r#""has-[[data-
foo]]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert!(!result.is_empty(), "newline in attr must not panic");
    }

    #[test]
    fn test_has_data_with_unicode() {
        // Unicode in attribute value — shouldn't happen in practice but must not panic
        let input = r#""has-[[data-label=héllo]]""#;
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, r#""has-data-[label=héllo]""#);
    }

    // =========================================================================
    // New: tokenize_regions unit tests
    // =========================================================================

    #[test]
    fn test_tokenize_simple_string() {
        let regions = tokenize_regions(r#"const x = "hello";"#);
        assert_eq!(regions.len(), 5);
        assert_eq!(
            regions[0],
            Region {
                text: "const x = ",
                kind: RegionKind::Code
            }
        );
        assert_eq!(
            regions[1],
            Region {
                text: "\"",
                kind: RegionKind::Code
            }
        ); // opening "
        assert_eq!(
            regions[2],
            Region {
                text: "hello",
                kind: RegionKind::StringContent
            }
        );
        assert_eq!(
            regions[3],
            Region {
                text: "\"",
                kind: RegionKind::Code
            }
        ); // closing "
        assert_eq!(
            regions[4],
            Region {
                text: ";",
                kind: RegionKind::Code
            }
        ); // trailing
    }

    #[test]
    fn test_tokenize_escaped_quote() {
        let regions = tokenize_regions(r#""he\"llo""#);
        // Opening ", then content he\"llo, then closing "
        let string_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::StringContent)
            .collect();
        assert_eq!(string_regions.len(), 1);
        assert_eq!(string_regions[0].text, r#"he\"llo"#);
    }

    #[test]
    fn test_tokenize_no_strings() {
        let regions = tokenize_regions("const x = 42;");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].kind, RegionKind::Code);
    }

    #[test]
    fn test_tokenize_empty() {
        let regions = tokenize_regions("");
        assert!(regions.is_empty());
    }

    #[test]
    fn test_tokenize_adjacent_strings() {
        let regions = tokenize_regions(r#""a" + "b""#);
        let string_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::StringContent)
            .collect();
        assert_eq!(string_regions.len(), 2);
        assert_eq!(string_regions[0].text, "a");
        assert_eq!(string_regions[1].text, "b");
    }

    #[test]
    fn test_tokenize_single_quotes() {
        let regions = tokenize_regions("const x = 'hello';");
        let string_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::StringContent)
            .collect();
        assert_eq!(string_regions.len(), 1);
        assert_eq!(string_regions[0].text, "hello");
    }

    #[test]
    fn test_tokenize_backtick() {
        let regions = tokenize_regions("const x = `hello`;");
        let string_regions: Vec<_> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::StringContent)
            .collect();
        assert_eq!(string_regions.len(), 1);
        assert_eq!(string_regions[0].text, "hello");
    }

    // =========================================================================
    // New: rfind_variant_colon unit tests
    // =========================================================================

    #[test]
    fn test_rfind_variant_colon_simple() {
        assert_eq!(rfind_variant_colon("hover:bg-red"), Some(5));
    }

    #[test]
    fn test_rfind_variant_colon_nested_brackets() {
        // group-data-[collapsible=icon]:!p-2
        // The colon inside [collapsible=icon] is at depth > 0
        let token = "group-data-[collapsible=icon]:!p-2";
        let result = rfind_variant_colon(token);
        assert_eq!(result, Some(29)); // the colon after ]
    }

    #[test]
    fn test_rfind_variant_colon_none() {
        assert_eq!(rfind_variant_colon("bg-red"), None);
    }

    #[test]
    fn test_rfind_variant_colon_multiple() {
        // dark:hover:bg-red → rightmost colon at depth 0
        let token = "dark:hover:bg-red";
        assert_eq!(rfind_variant_colon(token), Some(10));
    }

    // =========================================================================
    // New: transform_class_token Cow::Borrowed fast path
    // =========================================================================

    #[test]
    fn test_transform_class_token_borrowed_fast_path() {
        // A plain class like "flex" should return Cow::Borrowed (no allocation)
        let result = transform_class_token("flex");
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "plain class should be Borrowed"
        );
        assert_eq!(&*result, "flex");
    }

    #[test]
    fn test_transform_class_token_owned_on_transform() {
        // A class that triggers css_var_syntax should return Cow::Owned
        let result = transform_class_token("w-[--sidebar-width]");
        assert!(
            matches!(result, Cow::Owned(_)),
            "transformed class should be Owned"
        );
        assert_eq!(&*result, "w-(--sidebar-width)");
    }

    // =========================================================================
    // New: code outside strings is never transformed
    // =========================================================================

    #[test]
    fn test_code_not_transformed() {
        // JS negation !open outside of strings should not be touched
        let input = "if (!open) { return }";
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_css_var_in_code_not_transformed() {
        // [--foo] outside strings is code, should not be transformed
        let input = "const x = [--foo];";
        let result = transform_tailwind_v3_to_v4(input);
        assert_eq!(result, input);
    }
}
