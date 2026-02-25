use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::common::ProjectMetadata;

/// UI configuration derived from pyproject.toml [tool.apx.ui]
#[derive(Debug, Clone)]
pub struct UiConfig {
    /// Root directory of the frontend UI sources.
    pub root: PathBuf,
    /// Named component registries (local overrides and catalog entries).
    pub registries: HashMap<String, RegistryConfig>,
}

impl UiConfig {
    /// Construct UiConfig from ProjectMetadata
    pub fn from_metadata(metadata: &ProjectMetadata, app_dir: &Path) -> Result<Self, String> {
        let ui_root = metadata
            .ui_root
            .as_ref()
            .ok_or("Project has no UI configured (missing [tool.apx.ui] in pyproject.toml)")?;
        let root = app_dir.join(ui_root);

        // Convert string registries to RegistryConfig
        let registries: HashMap<String, RegistryConfig> = metadata
            .ui_registries
            .as_ref()
            .map(|regs| {
                regs.iter()
                    .map(|(k, v)| (k.clone(), RegistryConfig::Template(v.clone())))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self { root, registries })
    }

    /// Hardcoded shadcn style
    pub fn style(&self) -> &'static str {
        "new-york"
    }

    /// CSS file path: {root}/styles/globals.css
    pub fn css_path(&self) -> PathBuf {
        self.root.join("styles/globals.css")
    }

    /// Components dir: {root}/components
    pub fn components_dir(&self) -> PathBuf {
        self.root.join("components")
    }

    /// Lib dir: {root}/lib
    pub fn lib_dir(&self) -> PathBuf {
        self.root.join("lib")
    }

    /// Hooks dir: {root}/hooks
    pub fn hooks_dir(&self) -> PathBuf {
        self.root.join("hooks")
    }
}

/// A component registry configuration, either a simple URL template or an advanced config.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RegistryConfig {
    /// Simple URL template with `{name}` / `{style}` placeholders.
    Template(String),
    /// Advanced registry with custom headers and parameters.
    Advanced(RegistryAdvanced),
}

/// Advanced registry configuration with URL, headers, and query parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAdvanced {
    /// Base URL template.
    pub url: String,

    /// Extra HTTP headers to include.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Extra query / template parameters.
    #[serde(default)]
    pub params: HashMap<String, String>,
}

/// An entry from the upstream shadcn registry catalog.
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryCatalogEntry {
    /// Registry name (used as the `@name` prefix).
    pub name: String,
    /// URL template for fetching component specs.
    pub url: String,
    // #[serde(default)]
    // pub homepage: Option<String>,
}

/// CSS rules represented as a JSON object.
pub type CssRules = Map<String, Value>;

/// The type of a registry item.
#[derive(Debug, Deserialize, serde::Serialize, Clone, Copy)]
pub enum RegistryItemType {
    /// A block-level layout component.
    #[serde(rename = "registry:block")]
    Block,
    /// A UI component.
    #[serde(rename = "registry:component")]
    Component,
    /// A library utility module.
    #[serde(rename = "registry:lib")]
    Lib,
    /// A React hook.
    #[serde(rename = "registry:hook")]
    Hook,
    /// A UI primitive.
    #[serde(rename = "registry:ui")]
    Ui,
    /// A full page template.
    #[serde(rename = "registry:page")]
    Page,
    /// A standalone file.
    #[serde(rename = "registry:file")]
    File,
    /// A style definition.
    #[serde(rename = "registry:style")]
    Style,
    /// A theme definition.
    #[serde(rename = "registry:theme")]
    Theme,
    /// A generic registry item.
    #[serde(rename = "registry:item")]
    Item,
}

/// Component JSON (registry item)
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryItem {
    /// Component name.
    pub name: String,
    /// Human-readable title.
    #[serde(default)]
    pub title: Option<String>,
    /// Brief description.
    #[serde(default)]
    pub description: Option<String>,
    /// Type of registry item.
    #[serde(rename = "type")]
    pub item_type: RegistryItemType,
    /// Source files included in this item.
    pub files: Vec<RegistryFile>,

    /// npm package dependencies.
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Other registry components this item depends on.
    #[serde(default, rename = "registryDependencies")]
    pub registry_dependencies: Vec<String>,

    /// CSS custom property overrides.
    #[serde(default, rename = "cssVars")]
    pub css_vars: Option<CssVars>,

    /// Raw CSS rules to inject.
    #[serde(default)]
    pub css: Option<CssRules>,

    /// Deprecated Tailwind v3 config - converted to CSS for Tailwind v4.
    #[serde(default)]
    pub tailwind: Option<TailwindConfig>,

    /// Documentation URL.
    #[serde(default)]
    pub docs: Option<String>,

    /// Category tags for search and filtering.
    #[serde(default)]
    pub categories: Vec<String>,

    /// Arbitrary metadata.
    #[serde(default)]
    pub meta: Option<Value>,
}

/// CSS custom properties scoped to theme/light/dark modes.
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct CssVars {
    /// Base theme variables.
    #[serde(default)]
    pub theme: HashMap<String, String>,
    /// Light-mode overrides.
    #[serde(default)]
    pub light: HashMap<String, String>,
    /// Dark-mode overrides.
    #[serde(default)]
    pub dark: HashMap<String, String>,
}

/// Tailwind configuration from registry items (deprecated in Tailwind v4, but still used by many components)
/// Structure: { config: { theme: { extend: { colors, keyframes, animation } } } }
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindConfig {
    /// Optional inner config object.
    #[serde(default)]
    pub config: Option<TailwindConfigInner>,
}

/// Inner Tailwind configuration wrapping theme settings.
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindConfigInner {
    /// Theme customization block.
    #[serde(default)]
    pub theme: Option<TailwindTheme>,
}

/// Tailwind theme configuration.
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindTheme {
    /// Extended theme values (colors, keyframes, etc.).
    #[serde(default)]
    pub extend: Option<TailwindThemeExtend>,
}

/// Tailwind theme extensions (colors, keyframes, animations, etc.).
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindThemeExtend {
    /// Color definitions - can be simple or nested:
    /// Simple: { "brand": "hsl(var(--brand))" }
    /// Nested: { "sidebar": { "DEFAULT": "hsl(...)", "foreground": "hsl(...)" } }
    #[serde(default)]
    pub colors: HashMap<String, Value>,

    /// Keyframe definitions: { "accordion-down": { "from": {...}, "to": {...} } }
    /// Selectors can be "from"/"to" or percentages like "0%, 100%"
    #[serde(default)]
    pub keyframes: HashMap<String, HashMap<String, Value>>,

    /// Animation definitions: { "accordion-down": "accordion-down 0.2s ease-out" }
    #[serde(default)]
    pub animation: HashMap<String, String>,

    /// Font family definitions: `{ "heading": ["Poppins", "sans-serif"] }`
    /// Converted to @theme inline { --font-{name}: value; }
    #[serde(default, rename = "fontFamily")]
    pub font_family: HashMap<String, Value>,

    /// Border radius definitions: { "custom": "0.5rem" }
    /// Converted to @theme inline { --radius-{name}: value; }
    #[serde(default, rename = "borderRadius")]
    pub border_radius: HashMap<String, String>,

    /// Spacing definitions: { "custom": "2rem" }
    /// Converted to @theme inline { --spacing-{name}: value; }
    #[serde(default)]
    pub spacing: HashMap<String, String>,
}

/// A source file within a registry item.
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryFile {
    /// Relative output path for this file.
    pub path: String,
    /// File content.
    pub content: String,

    /// Optional file type hint.
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
}
