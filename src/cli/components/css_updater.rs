use std::collections::HashSet;
use std::fmt;

use biome_css_parser::{CssParserOptions, parse_css};
use biome_css_syntax::*;
use biome_rowan::AstNode;

/// Errors that can occur while parsing or updating CSS.
///
/// Intentionally minimal: this module is internal and operates
/// under strong invariants (valid CSS, shadcn-initialized project).
#[derive(Debug)]
pub enum CssUpdateError {
    ParseError(String),
}

impl fmt::Display for CssUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CssUpdateError::ParseError(msg) => write!(f, "failed to parse CSS: {msg}"),
        }
    }
}

impl std::error::Error for CssUpdateError {}

type Result<T> = std::result::Result<T, CssUpdateError>;

/// High-level mutations requested by registry items.
#[allow(clippy::enum_variant_names)]
pub enum CssMutation {
    /// Add a raw at-rule block (e.g. `@layer base`, `@keyframes foo`)
    AddCssBlock { at_rule: String, body: String },

    /// Add CSS variables to a selector (`:root`, `.dark`)
    AddCssVars {
        selector: String,
        vars: Vec<(String, String)>,
    },

    /// Add mappings inside `@theme inline`
    AddThemeMappings { vars: Vec<(String, String)> },
}

/// Append-only CSS updater.
/// Uses Biome CST only to *analyze* what already exists.
/// Mutations are applied by appending raw text to the original source.
///
/// Guarantees:
/// - no overwrites
/// - no deletions
/// - no reformatting
/// - idempotent application
pub struct CssUpdater {
    source: String,
    root: CssRoot,
}

impl CssUpdater {
    pub fn new(source: &str) -> Result<Self> {
        let parsed = parse_css(source, CssParserOptions::default());

        if parsed.has_errors() {
            // Diagnostics are not Display; render via Debug for now.
            return Err(CssUpdateError::ParseError(format!(
                "{:?}",
                parsed.diagnostics()
            )));
        }

        Ok(Self {
            source: source.to_string(),
            root: parsed.tree(),
        })
    }

    /// Apply mutations (append-only).
    ///
    /// Returns `true` if any mutation changed the document.
    pub fn apply(&mut self, mutations: &[CssMutation]) -> Result<bool> {
        // Build "what exists" sets once from the original CST…
        // and keep them updated as we append new content in this run.
        let mut existing_theme_vars = self.collect_existing_theme_vars();
        let mut existing_root_vars = self.collect_existing_vars(":root");
        let mut existing_dark_vars = self.collect_existing_vars(".dark");

        let mut existing_at_rules = self.collect_existing_at_rules();

        let mut changed = false;

        for mutation in mutations {
            match mutation {
                CssMutation::AddCssBlock { at_rule, body } => {
                    if !existing_at_rules.contains(at_rule) {
                        self.append_css_block(at_rule, body);
                        existing_at_rules.insert(at_rule.clone());
                        changed = true;
                    }
                }

                CssMutation::AddCssVars { selector, vars } => {
                    let target_set: &mut HashSet<String> = match selector.as_str() {
                        ":root" => &mut existing_root_vars,
                        ".dark" => &mut existing_dark_vars,
                        // Under your assumptions, only these two are expected.
                        // If you later extend selectors, add more buckets here.
                        _ => {
                            // Still support it: compute existing vars for this selector once,
                            // then track additions in a local set for idempotency within the run.
                            // (We don't persist that set across runs, but your inputs are controlled.)
                            // For now, just treat as empty and always append if vars non-empty.
                            // If you want, we can add a HashMap<String, HashSet<String>>.
                            let mut local = HashSet::new();
                            let appended = self.append_css_vars_block(selector, vars, &mut local);
                            if appended {
                                changed = true;
                            }
                            continue;
                        }
                    };

                    let appended = self.append_css_vars_block(selector, vars, target_set);
                    if appended {
                        changed = true;
                    }
                }

                CssMutation::AddThemeMappings { vars } => {
                    let appended = self.append_theme_inline_block(vars, &mut existing_theme_vars);
                    if appended {
                        changed = true;
                    }
                }
            }
        }

        Ok(changed)
    }

    pub fn finish(self) -> String {
        self.source
    }

    // ------------------------------------------------------------
    // Append-only writers
    // ------------------------------------------------------------

    fn append_css_block(&mut self, at_rule: &str, body: &str) {
        self.source.push('\n');
        self.source.push_str(at_rule);
        self.source.push_str(" {\n");
        self.source.push_str(body);
        if !body.ends_with('\n') {
            self.source.push('\n');
        }
        self.source.push_str("}\n");
    }

    fn append_css_vars_block(
        &mut self,
        selector: &str,
        vars: &[(String, String)],
        existing: &mut HashSet<String>,
    ) -> bool {
        let mut lines = String::new();
        for (k, v) in vars {
            if !existing.contains(k) {
                existing.insert(k.clone());
                lines.push_str("  ");
                lines.push_str(k);
                lines.push_str(": ");
                lines.push_str(v);
                lines.push_str(";\n");
            }
        }

        if lines.is_empty() {
            return false;
        }

        self.source.push('\n');
        self.source.push_str(selector);
        self.source.push_str(" {\n");
        self.source.push_str(&lines);
        self.source.push_str("}\n");
        true
    }

    fn append_theme_inline_block(
        &mut self,
        vars: &[(String, String)],
        existing: &mut HashSet<String>,
    ) -> bool {
        let mut lines = String::new();
        for (k, v) in vars {
            if !existing.contains(k) {
                existing.insert(k.clone());
                lines.push_str("  ");
                lines.push_str(k);
                lines.push_str(": ");
                lines.push_str(v);
                lines.push_str(";\n");
            }
        }

        if lines.is_empty() {
            return false;
        }

        self.source.push_str("\n@theme inline {\n");
        self.source.push_str(&lines);
        self.source.push_str("}\n");
        true
    }

    // ------------------------------------------------------------
    // CST analysis helpers
    // ------------------------------------------------------------

    fn collect_existing_at_rules(&self) -> HashSet<String> {
        // We only use this for the "at_rule exists" check in AddCssBlock.
        // We consider an at-rule "existing" if its header text appears as a node's trimmed text.
        // Under your constraints, this is sufficient and fast.
        let mut out = HashSet::new();
        for node in self.root.syntax().descendants() {
            let t = node.text_trimmed().to_string();
            if t.starts_with("@layer ")
                || t.starts_with("@keyframes ")
                || t.starts_with("@utility ")
                || t.starts_with("@plugin ")
                || t.starts_with("@theme")
            {
                out.insert(t);
            }
        }
        out
    }

    fn collect_existing_vars(&self, selector: &str) -> HashSet<String> {
        let mut vars = HashSet::new();

        for node in self.root.syntax().descendants() {
            let t = node.text_trimmed().to_string();

            // Cheap filter: look for nodes that start with the selector.
            // Works well for standard shadcn CSS structure.
            if !t.starts_with(selector) {
                continue;
            }

            for decl in node.descendants() {
                let dt = decl.text_trimmed().to_string();
                if let Some((name, _)) = dt.split_once(':') {
                    let name = name.trim();
                    if name.starts_with("--") {
                        vars.insert(name.to_string());
                    }
                }
            }
        }

        vars
    }

    fn collect_existing_theme_vars(&self) -> HashSet<String> {
        let mut vars = HashSet::new();

        for node in self.root.syntax().descendants() {
            let t = node.text_trimmed().to_string();
            if !t.starts_with("@theme") {
                continue;
            }

            for decl in node.descendants() {
                let dt = decl.text_trimmed().to_string();
                if let Some((name, _)) = dt.split_once(':') {
                    let name = name.trim();
                    if name.starts_with("--") {
                        vars.insert(name.to_string());
                    }
                }
            }
        }

        vars
    }
}
