//! Tailwind CSS v3 to v4 class syntax transformation utilities.
//!
//! Architecture: source is tokenized into Code/StringContent regions, then only
//! StringContent regions have per-token class transforms applied. Each transform
//! is a domain type implementing `Parse` + `Display` — parsing validates the v3
//! pattern and `Display` renders the v4 output.
//!
//! Key abstractions:
//! - `ByteScanner` / `ReverseByteScanner` — zero-copy byte cursors replacing raw indexing
//! - `ClassCheck` enum + `TailwindClassDetector` — composable class validation
//! - `AttrBoundary` — domain type for `]]`-terminated attribute scanning
//! - `TransformedRegion` — value type for the functional transform pipeline

use std::fmt;

// ─── CSS syntax tokens ──────────────────────────────────────────────────────

/// Single-byte tokens that carry meaning in CSS class / Tailwind syntax.
///
/// Every byte literal used in scanning or matching logic lives here.
/// No bare `b'…'` or `'…'` literals should appear outside this enum.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CssSyntax {
    /// `[` — opens an arbitrary value or attribute selector.
    OpenBracket,
    /// `]` — closes an arbitrary value or attribute selector.
    CloseBracket,
    /// `(` — opens a function call or grouping (e.g. `calc(…)`).
    OpenParen,
    /// `)` — closes a function call or grouping.
    CloseParen,
    /// `:` — Tailwind variant separator (e.g. `hover:`, `dark:`).
    Colon,
    /// `!` — Tailwind v3 important modifier prefix.
    Important,
    /// `-` — hyphen, separates Tailwind utility segments (e.g. `p-2`).
    Hyphen,
    /// `\` — escape character inside JS/TS string literals.
    Escape,
    /// `>` — CSS child combinator.
    ChildCombinator,
    /// `+` — CSS adjacent sibling combinator.
    AdjacentSibling,
    /// `~` — CSS general sibling combinator.
    GeneralSibling,
    /// ` ` — space, both a token separator and a CSS descendant combinator.
    Space,
}

impl CssSyntax {
    /// The raw byte value of this syntax token.
    const fn byte(self) -> u8 {
        match self {
            Self::OpenBracket => b'[',
            Self::CloseBracket => b']',
            Self::OpenParen => b'(',
            Self::CloseParen => b')',
            Self::Colon => b':',
            Self::Important => b'!',
            Self::Hyphen => b'-',
            Self::Escape => b'\\',
            Self::ChildCombinator => b'>',
            Self::AdjacentSibling => b'+',
            Self::GeneralSibling => b'~',
            Self::Space => b' ',
        }
    }

    /// The `char` value, for use with `str` methods like `contains` and `strip_prefix`.
    const fn char(self) -> char {
        self.byte() as char
    }

    /// Identify a byte as a known CSS syntax token.
    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'[' => Some(Self::OpenBracket),
            b']' => Some(Self::CloseBracket),
            b'(' => Some(Self::OpenParen),
            b')' => Some(Self::CloseParen),
            b':' => Some(Self::Colon),
            b'!' => Some(Self::Important),
            b'-' => Some(Self::Hyphen),
            b'\\' => Some(Self::Escape),
            b'>' => Some(Self::ChildCombinator),
            b'+' => Some(Self::AdjacentSibling),
            b'~' => Some(Self::GeneralSibling),
            b' ' => Some(Self::Space),
            _ => None,
        }
    }
}

// ─── Named constants ─────────────────────────────────────────────────────────

/// Byte values recognized as string delimiters in JS/TS/JSX source.
const QUOTE_BYTES: [u8; 3] = [b'"', b'\'', b'`'];

/// ASCII whitespace bytes that separate Tailwind class tokens.
const WHITESPACE_BYTES: [u8; 4] = [CssSyntax::Space.byte(), b'\t', b'\n', b'\r'];

/// Bytes that indicate a complex CSS selector inside `has-[...]`,
/// meaning the content is NOT a simple v3 data-attribute shorthand.
const COMPLEX_SELECTOR_BYTES: [u8; 8] = [
    CssSyntax::OpenBracket.byte(),
    CssSyntax::Colon.byte(),
    CssSyntax::Space.byte(),
    CssSyntax::ChildCombinator.byte(),
    CssSyntax::AdjacentSibling.byte(),
    CssSyntax::GeneralSibling.byte(),
    CssSyntax::OpenParen.byte(),
    CssSyntax::CloseParen.byte(),
];

/// Byte length of the `]]` closing pair in data attribute shorthands.
const DOUBLE_CLOSE_BRACKET_LEN: usize = 2;

/// Prefix inside a bracket that signals a CSS custom property: `[--`.
const CSS_VAR_BRACKET_PREFIX: &str = "[--";

/// CSS custom property double-dash prefix inside bracket content.
const CSS_VAR_DOUBLE_DASH: &str = "--";

/// Pre-computed search needles for `DataShorthand`, avoiding per-iteration `format!`.
/// Each entry is `(needle, prefix)` where needle = `"{prefix}[[data-"`.
const DATA_SHORTHAND_NEEDLES: [(&str, &str); 3] = [
    ("has-[[data-", "has-"),
    ("group-has-[[data-", "group-has-"),
    ("peer-has-[[data-", "peer-has-"),
];

// ─── ByteScanner ────────────────────────────────────────────────────────────

/// Zero-copy forward cursor over a byte slice.
///
/// Replaces all manual `bytes[i]` / `i += 1` patterns with domain-level
/// scanning operations. Each method is a single-purpose, stateless query
/// or a minimal position advance.
struct ByteScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteScanner<'a> {
    /// Create a scanner starting at byte 0.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Create a scanner starting at a specific byte offset.
    fn at(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    /// Look at the current byte without advancing.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// Move forward one byte.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Current byte offset.
    fn position(&self) -> usize {
        self.pos
    }

    /// True when the cursor is past the last byte.
    fn is_exhausted(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Advance while `predicate` holds. Does not consume the first
    /// byte that fails the predicate.
    fn skip_while(&mut self, predicate: impl Fn(u8) -> bool) {
        while self.pos < self.bytes.len() && predicate(self.bytes[self.pos]) {
            self.pos += 1;
        }
    }

    /// Extract a substring from `source` spanning `[from, current position)`.
    fn slice_from<'s>(&self, source: &'s str, from: usize) -> &'s str {
        &source[from..self.pos]
    }
}

// ─── ReverseByteScanner ─────────────────────────────────────────────────────

/// Zero-copy right-to-left cursor over a byte slice.
///
/// Used for bracket-aware reverse scanning (e.g. finding the rightmost
/// variant colon separator in a Tailwind token).
struct ReverseByteScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ReverseByteScanner<'a> {
    /// Create a reverse scanner starting past the last byte.
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: bytes.len(),
        }
    }

    /// Yield the next `(index, byte)` pair moving right-to-left.
    /// Returns `None` when the beginning is reached.
    fn next(&mut self) -> Option<(usize, u8)> {
        if self.pos == 0 {
            return None;
        }
        self.pos -= 1;
        Some((self.pos, self.bytes[self.pos]))
    }
}

// ─── TailwindClassDetector ──────────────────────────────────────────────────

/// Individual validation rules for identifying a Tailwind class token.
/// Each variant is a named, self-documenting check.
#[derive(Clone, Copy)]
enum ClassCheck {
    /// Token must be non-empty.
    NonEmpty,
    /// First byte must be an ASCII letter (a-z, A-Z).
    StartsWithLetter,
    /// Token must contain at least one hyphen (Tailwind utilities are hyphenated).
    ContainsHyphen,
}

impl ClassCheck {
    fn passes(self, token: &str) -> bool {
        match self {
            Self::NonEmpty => !token.is_empty(),
            Self::StartsWithLetter => token
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic()),
            Self::ContainsHyphen => token.contains(CssSyntax::Hyphen.char()),
        }
    }
}

/// Ordered set of checks that identify a likely Tailwind class token.
const TAILWIND_CLASS_CHECKS: &[ClassCheck] = &[
    ClassCheck::NonEmpty,
    ClassCheck::StartsWithLetter,
    ClassCheck::ContainsHyphen,
];

/// Composite detector: all `TAILWIND_CLASS_CHECKS` must pass.
struct TailwindClassDetector;

impl TailwindClassDetector {
    fn is_match(token: &str) -> bool {
        TAILWIND_CLASS_CHECKS
            .iter()
            .all(|check| check.passes(token))
    }
}

// ─── AttrBoundary ───────────────────────────────────────────────────────────

/// Result of scanning for a simple `]]`-terminated data attribute.
///
/// Produced by scanning the content after `has-[[data-` for a simple
/// `WORD` or `WORD=VALUE` pattern that ends with `]]`.
struct AttrBoundary {
    /// Byte offset of the first `]` in the closing `]]`.
    end_offset: usize,
}

impl AttrBoundary {
    /// The attribute name/value portion before the closing `]]`.
    fn attr_content<'a>(&self, content: &'a str) -> &'a str {
        &content[..self.end_offset]
    }

    /// Everything after the closing `]]`.
    fn after_double_close<'a>(&self, content: &'a str) -> &'a str {
        &content[self.end_offset + DOUBLE_CLOSE_BRACKET_LEN..]
    }

    /// Scan `content` for a simple data attribute ending with `]]`.
    ///
    /// Returns `None` if the content is empty, contains complex selector bytes
    /// (`[`, `:`, ` `, `>`, `+`, `~`, `(`, `)`), or has no `]]` terminator.
    fn scan(content: &str) -> Option<Self> {
        let mut scanner = ByteScanner::new(content.as_bytes());
        while let Some(b) = scanner.peek() {
            if b == CssSyntax::CloseBracket.byte() {
                return Self::validate_double_close(&scanner);
            }
            if COMPLEX_SELECTOR_BYTES.contains(&b) {
                return None;
            }
            scanner.advance();
        }
        None
    }

    /// Check that the scanner is at a `]]` pair and that the attribute name is non-empty.
    fn validate_double_close(scanner: &ByteScanner<'_>) -> Option<Self> {
        let pos = scanner.position();
        // Empty attribute name (e.g. `has-[[data-]]`)
        if pos == 0 {
            return None;
        }
        // Must be followed by another `]` to form `]]`
        let next_is_close =
            scanner.bytes.get(pos + 1).copied() == Some(CssSyntax::CloseBracket.byte());
        if next_is_close {
            Some(AttrBoundary { end_offset: pos })
        } else {
            None
        }
    }
}

// ─── Parse trait ─────────────────────────────────────────────────────────────

/// A domain type that can be parsed from a Tailwind class token and displayed
/// in v4 syntax. `parse` returns `Some(Self)` if the token matches the v3
/// pattern, `None` otherwise.
trait Parse<'a>: Sized + fmt::Display {
    fn parse(token: &'a str) -> Option<Self>;
}

// ─── TransformedRegion ──────────────────────────────────────────────────────

/// A region (or token) after transformation — either unchanged or rewritten.
///
/// Explicit domain type (not `Cow`) to distinguish "no transform needed"
/// from "transform produced identical text".
enum TransformedRegion<'a> {
    /// Region passed through without modification.
    Unchanged(&'a str),
    /// Region was rewritten by a v3→v4 transform.
    Rewritten(String),
}

impl TransformedRegion<'_> {
    fn as_str(&self) -> &str {
        match self {
            TransformedRegion::Unchanged(s) => s,
            TransformedRegion::Rewritten(s) => s.as_str(),
        }
    }
}

// ─── BracketPair ────────────────────────────────────────────────────────────

/// Matched `[`…`]` pair found by depth-tracking scan.
struct BracketPair {
    open: usize,
    close: usize,
}

impl BracketPair {
    /// The text between the brackets, exclusive of `[` and `]`.
    fn content<'a>(&self, source: &'a str) -> &'a str {
        &source[self.open + 1..self.close]
    }

    /// Everything before the opening `[`.
    fn prefix<'a>(&self, source: &'a str) -> &'a str {
        &source[..self.open]
    }

    /// Everything after the closing `]`.
    fn suffix<'a>(&self, source: &'a str) -> &'a str {
        &source[self.close + 1..]
    }
}

// ─── CssVar ──────────────────────────────────────────────────────────────────

/// CSS custom property bracket syntax: `w-[--sidebar-width]` → `w-(--sidebar-width)`
struct CssVar<'a> {
    prefix: &'a str,
    var_name: &'a str,
    suffix: &'a str,
}

impl<'a> Parse<'a> for CssVar<'a> {
    fn parse(token: &'a str) -> Option<Self> {
        let open = token.find(CSS_VAR_BRACKET_PREFIX)?;
        let brackets = find_matching_close_bracket(token, open)?;
        let var_name = brackets.content(token);
        if var_name.starts_with(CSS_VAR_DOUBLE_DASH)
            && !var_name.contains(CssSyntax::OpenParen.char())
        {
            Some(CssVar {
                prefix: brackets.prefix(token),
                var_name,
                suffix: brackets.suffix(token),
            })
        } else {
            None
        }
    }
}

impl fmt::Display for CssVar<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}){}", self.prefix, self.var_name, self.suffix)
    }
}

// ─── DataShorthand ───────────────────────────────────────────────────────────

/// V3 has-data attribute shorthand: `has-[[data-variant=inset]]` → `has-data-[variant=inset]`
struct DataShorthand<'a> {
    before: &'a str,
    has_variant: &'a str,
    attr: &'a str,
    suffix: &'a str,
}

impl<'a> Parse<'a> for DataShorthand<'a> {
    fn parse(token: &'a str) -> Option<Self> {
        for &(needle, has_variant) in &DATA_SHORTHAND_NEEDLES {
            let Some((before, after)) = token.split_once(needle) else {
                continue;
            };
            let Some(boundary) = AttrBoundary::scan(after) else {
                continue;
            };
            return Some(DataShorthand {
                before,
                has_variant,
                attr: boundary.attr_content(after),
                suffix: boundary.after_double_close(after),
            });
        }
        None
    }
}

impl fmt::Display for DataShorthand<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}data-[{}]{}",
            self.before, self.has_variant, self.attr, self.suffix
        )
    }
}

// ─── VariantSplit ────────────────────────────────────────────────────────────

/// Result of splitting a Tailwind token at the rightmost variant colon.
///
/// E.g. `"hover:bg-red"` → prefix `"hover:"`, after_colon `"bg-red"`.
struct VariantSplit<'a> {
    /// Everything up to and including the colon (e.g. `"group-data-[x]:"`).
    prefix: &'a str,
    /// Everything after the colon (e.g. `"!p-2"`).
    after_colon: &'a str,
}

// ─── ImportantModifier ───────────────────────────────────────────────────────

/// Important modifier from prefix to suffix: `!p-2` → `p-2!`
struct ImportantModifier<'a> {
    variant_prefix: &'a str,
    class_name: &'a str,
}

impl<'a> ImportantModifier<'a> {
    /// Try `!class-name` at the start of the token (no variant prefix).
    fn parse_bare(token: &'a str) -> Option<Self> {
        let class = token.strip_prefix(CssSyntax::Important.char())?;
        TailwindClassDetector::is_match(class).then_some(ImportantModifier {
            variant_prefix: "",
            class_name: class,
        })
    }

    /// Try `variant:!class-name` — important after the rightmost variant colon.
    fn parse_after_variant(token: &'a str) -> Option<Self> {
        let split = rfind_variant_colon(token)?;
        let class = split
            .after_colon
            .strip_prefix(CssSyntax::Important.char())
            .filter(|c| TailwindClassDetector::is_match(c))?;
        Some(ImportantModifier {
            variant_prefix: split.prefix,
            class_name: class,
        })
    }
}

impl<'a> Parse<'a> for ImportantModifier<'a> {
    fn parse(token: &'a str) -> Option<Self> {
        Self::parse_bare(token).or_else(|| Self::parse_after_variant(token))
    }
}

impl fmt::Display for ImportantModifier<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}!", self.variant_prefix, self.class_name)
    }
}

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

// ─── Region tokenizer ────────────────────────────────────────────────────────

/// A byte range within a source string, paired with a region kind.
struct Span {
    start: usize,
    end: usize,
    kind: RegionKind,
}

impl Span {
    /// Convert this span into a `Region` borrowing from `source`.
    /// Returns `None` if the span is empty (start == end).
    fn into_region(self, source: &str) -> Option<Region<'_>> {
        if self.end > self.start {
            Some(Region {
                text: &source[self.start..self.end],
                kind: self.kind,
            })
        } else {
            None
        }
    }
}

/// Splits JS/TS source into Code and StringContent regions.
///
/// Recognizes `"`, `'`, and `` ` `` as string delimiters (handling `\` escapes).
/// Delimiter characters belong to Code regions so transforms never see quotes.
struct RegionTokenizer<'a> {
    input: &'a str,
    scanner: ByteScanner<'a>,
    regions: Vec<Region<'a>>,
    region_start: usize,
}

impl<'a> RegionTokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            scanner: ByteScanner::new(input.as_bytes()),
            regions: Vec::new(),
            region_start: 0,
        }
    }

    fn tokenize(mut self) -> Vec<Region<'a>> {
        while let Some(b) = self.scanner.peek() {
            if QUOTE_BYTES.contains(&b) {
                self.process_quoted_string(b);
            } else {
                self.scanner.advance();
            }
        }
        self.emit_span(self.input.len(), RegionKind::Code);
        self.regions
    }

    fn process_quoted_string(&mut self, quote: u8) {
        let quote_pos = self.scanner.position();
        self.emit_span(quote_pos, RegionKind::Code);

        // Opening quote as Code
        self.region_start = quote_pos;
        self.scanner.advance();
        self.emit_span(self.scanner.position(), RegionKind::Code);

        // String content until closing quote
        self.scan_to_closing_quote(quote);
        self.emit_span(self.scanner.position(), RegionKind::StringContent);

        // Closing quote as Code (if found)
        if !self.scanner.is_exhausted() {
            self.scanner.advance();
            self.emit_span(self.scanner.position(), RegionKind::Code);
        }
    }

    fn scan_to_closing_quote(&mut self, quote: u8) {
        while let Some(b) = self.scanner.peek() {
            if b == CssSyntax::Escape.byte() {
                self.scanner.advance();
                self.scanner.advance();
                continue;
            }
            if b == quote {
                return;
            }
            self.scanner.advance();
        }
    }

    /// Emit a region from `region_start` to `end`, then advance `region_start`.
    fn emit_span(&mut self, end: usize, kind: RegionKind) {
        let span = Span {
            start: self.region_start,
            end,
            kind,
        };
        self.regions.extend(span.into_region(self.input));
        self.region_start = end;
    }
}

/// Split source into alternating Code / StringContent regions.
fn tokenize_regions(input: &str) -> Vec<Region<'_>> {
    RegionTokenizer::new(input).tokenize()
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
    let capacity = content.len();
    regions
        .iter()
        .map(|region| match region.kind {
            RegionKind::Code => TransformedRegion::Unchanged(region.text),
            RegionKind::StringContent => transform_string_region(region.text),
        })
        .fold(String::with_capacity(capacity), |mut acc, region| {
            acc.push_str(region.as_str());
            acc
        })
}

/// Transform all whitespace-separated class tokens in a string region.
///
/// Preserves original whitespace between tokens. Returns `Unchanged` if
/// no token was rewritten, avoiding unnecessary allocation.
fn transform_string_region(s: &str) -> TransformedRegion<'_> {
    let mut scanner = ByteScanner::new(s.as_bytes());
    let mut parts: Vec<TransformedRegion<'_>> = Vec::new();
    let mut any_rewritten = false;

    while !scanner.is_exhausted() {
        // Consume whitespace
        let ws_start = scanner.position();
        scanner.skip_while(|b| WHITESPACE_BYTES.contains(&b));
        let ws = scanner.slice_from(s, ws_start);
        if !ws.is_empty() {
            parts.push(TransformedRegion::Unchanged(ws));
        }
        if scanner.is_exhausted() {
            break;
        }

        // Consume non-whitespace token
        let tok_start = scanner.position();
        scanner.skip_while(|b| !WHITESPACE_BYTES.contains(&b));
        let token = scanner.slice_from(s, tok_start);

        let transformed = transform_token(token);
        if matches!(&transformed, TransformedRegion::Rewritten(_)) {
            any_rewritten = true;
        }
        parts.push(transformed);
    }

    if !any_rewritten {
        return TransformedRegion::Unchanged(s);
    }
    let assembled = parts.iter().map(|p| p.as_str()).collect::<String>();
    TransformedRegion::Rewritten(assembled)
}

/// Apply v3→v4 transforms to a single class token.
///
/// Tries each domain parser in priority order — transforms are
/// mutually exclusive for real Tailwind tokens.
fn transform_token(token: &str) -> TransformedRegion<'_> {
    if let Some(v) = CssVar::parse(token) {
        return TransformedRegion::Rewritten(v.to_string());
    }
    if let Some(v) = DataShorthand::parse(token) {
        return TransformedRegion::Rewritten(v.to_string());
    }
    if let Some(v) = ImportantModifier::parse(token) {
        return TransformedRegion::Rewritten(v.to_string());
    }
    TransformedRegion::Unchanged(token)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Find the matching `]` for the `[` at `open_pos`, tracking bracket depth.
fn find_matching_close_bracket(source: &str, open_pos: usize) -> Option<BracketPair> {
    let mut scanner = ByteScanner::at(source.as_bytes(), open_pos);
    let mut depth: i32 = 0;
    while let Some(b) = scanner.peek() {
        match CssSyntax::from_byte(b) {
            Some(CssSyntax::OpenBracket) => depth += 1,
            Some(CssSyntax::CloseBracket) => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(BracketPair {
                open: open_pos,
                close: scanner.position(),
            });
        }
        scanner.advance();
    }
    None
}

/// Bracket-aware right-to-left scan for the rightmost variant colon.
///
/// Returns the split at the rightmost `:` at bracket depth 0,
/// i.e. a true Tailwind variant separator (not inside `[...]` or `(...)`).
fn rfind_variant_colon(token: &str) -> Option<VariantSplit<'_>> {
    let mut scanner = ReverseByteScanner::new(token.as_bytes());
    let mut depth: i32 = 0;

    while let Some((pos, b)) = scanner.next() {
        match CssSyntax::from_byte(b) {
            Some(CssSyntax::CloseBracket | CssSyntax::CloseParen) => depth += 1,
            Some(CssSyntax::OpenBracket | CssSyntax::OpenParen) => depth -= 1,
            Some(CssSyntax::Colon) if depth == 0 => {
                return Some(VariantSplit {
                    prefix: &token[..=pos],
                    after_colon: &token[pos + 1..],
                });
            }
            _ => {}
        }
    }
    None
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
    // Unit tests for AttrBoundary::scan
    // =========================================================================

    #[test]
    fn test_find_simple_attr_with_value() {
        // "variant=inset]]..." → Some(13) pointing to first ]
        assert_eq!(
            AttrBoundary::scan("variant=inset]]").map(|b| b.end_offset),
            Some(13)
        );
    }

    #[test]
    fn test_find_simple_attr_no_value() {
        // "active]]" → Some(6)
        assert_eq!(
            AttrBoundary::scan("active]]").map(|b| b.end_offset),
            Some(6)
        );
    }

    #[test]
    fn test_find_complex_attr_with_pseudo() {
        // "slot=input-group-control]:focus-visible]" — has single ] then more content
        assert_eq!(
            AttrBoundary::scan("slot=input-group-control]:focus-visible]").map(|b| b.end_offset),
            None
        );
    }

    #[test]
    fn test_find_compound_attrs() {
        // "slot][aria-invalid=true]]" — has inner [ which is complex
        assert_eq!(
            AttrBoundary::scan("slot][aria-invalid=true]]").map(|b| b.end_offset),
            None
        );
    }

    #[test]
    fn test_find_empty() {
        // "]]" → None (empty attribute name)
        assert_eq!(AttrBoundary::scan("]]").map(|b| b.end_offset), None);
    }

    #[test]
    fn test_find_no_closing() {
        // "variant=inset" → None (no ]])
        assert_eq!(
            AttrBoundary::scan("variant=inset").map(|b| b.end_offset),
            None
        );
    }

    #[test]
    fn test_find_single_bracket_only() {
        // "variant=inset]" → None (only single ])
        assert_eq!(
            AttrBoundary::scan("variant=inset]").map(|b| b.end_offset),
            None
        );
    }

    // =========================================================================
    // Corner cases: malformed input, truncation, prefix collisions
    // =========================================================================

    #[test]
    fn test_has_data_truncated_no_closing_brackets() {
        // Malformed: has-[[data-variant=inset without closing ]]
        // With region tokenizer, the string content is processed per-token.
        // The token has-[[data-variant=inset has no ]] so AttrBoundary::scan returns None.
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
        // AttrBoundary::scan sees "]" at position 0 → validate_double_close → pos==0 returns None
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
            AttrBoundary::scan("slot=item-description]]").map(|b| b.end_offset),
            Some(21)
        );
    }

    #[test]
    fn test_find_attr_with_dots_and_numbers() {
        // Values can contain dots, numbers, etc.
        assert_eq!(
            AttrBoundary::scan("size=1.5]]").map(|b| b.end_offset),
            Some(8)
        );
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
        // CssVar::parse finds [-- but no matching ] → returns None
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
    // tokenize_regions unit tests
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
    // rfind_variant_colon unit tests
    // =========================================================================

    #[test]
    fn test_rfind_variant_colon_simple() {
        let result = rfind_variant_colon("hover:bg-red");
        assert_eq!(result.as_ref().map(|s| s.prefix), Some("hover:"));
        assert_eq!(result.as_ref().map(|s| s.after_colon), Some("bg-red"));
    }

    #[test]
    fn test_rfind_variant_colon_nested_brackets() {
        // group-data-[collapsible=icon]:!p-2
        // The colon inside [collapsible=icon] is at depth > 0
        let result = rfind_variant_colon("group-data-[collapsible=icon]:!p-2");
        assert_eq!(
            result.as_ref().map(|s| s.prefix),
            Some("group-data-[collapsible=icon]:")
        );
        assert_eq!(result.as_ref().map(|s| s.after_colon), Some("!p-2"));
    }

    #[test]
    fn test_rfind_variant_colon_none() {
        assert!(rfind_variant_colon("bg-red").is_none());
    }

    #[test]
    fn test_rfind_variant_colon_multiple() {
        // dark:hover:bg-red → rightmost colon at depth 0
        let result = rfind_variant_colon("dark:hover:bg-red");
        assert_eq!(result.as_ref().map(|s| s.prefix), Some("dark:hover:"));
        assert_eq!(result.as_ref().map(|s| s.after_colon), Some("bg-red"));
    }

    // =========================================================================
    // transform_token unit tests
    // =========================================================================

    #[test]
    fn test_transform_token_passthrough() {
        // A plain class like "flex" should pass through unchanged
        let result = transform_token("flex");
        assert_eq!(result.as_str(), "flex");
    }

    #[test]
    fn test_transform_token_css_var() {
        // A class that triggers CssVar should be transformed
        let result = transform_token("w-[--sidebar-width]");
        assert_eq!(result.as_str(), "w-(--sidebar-width)");
    }

    // =========================================================================
    // Code outside strings is never transformed
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
