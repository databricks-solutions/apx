use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::common::ProjectMetadata;

/// UI configuration derived from pyproject.toml [tool.apx.ui]
#[derive(Debug, Clone)]
pub struct UiConfig {
    pub root: PathBuf,
    pub registries: HashMap<String, RegistryConfig>,
}

impl UiConfig {
    /// Construct UiConfig from ProjectMetadata
    pub fn from_metadata(metadata: &ProjectMetadata, app_dir: &Path) -> Self {
        let root = app_dir.join(&metadata.ui_root);

        // Convert string registries to RegistryConfig
        let registries: HashMap<String, RegistryConfig> = metadata
            .ui_registries
            .iter()
            .map(|(k, v)| (k.clone(), RegistryConfig::Template(v.clone())))
            .collect();

        Self { root, registries }
    }

    /// Hardcoded shadcn style
    pub fn style(&self) -> &str {
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

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RegistryConfig {
    Template(String),
    Advanced(RegistryAdvanced),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAdvanced {
    pub url: String,

    #[serde(default)]
    pub headers: HashMap<String, String>,

    #[serde(default)]
    pub params: HashMap<String, String>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryCatalogEntry {
    pub name: String,
    pub url: String,
    // #[serde(default)]
    // pub homepage: Option<String>,
}

pub type CssRules = Map<String, Value>;

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub enum RegistryItemType {
    #[serde(rename = "registry:block")]
    Block,
    #[serde(rename = "registry:component")]
    Component,
    #[serde(rename = "registry:lib")]
    Lib,
    #[serde(rename = "registry:hook")]
    Hook,
    #[serde(rename = "registry:ui")]
    Ui,
    #[serde(rename = "registry:page")]
    Page,
    #[serde(rename = "registry:file")]
    File,
    #[serde(rename = "registry:style")]
    Style,
    #[serde(rename = "registry:theme")]
    Theme,
    #[serde(rename = "registry:item")]
    Item,
}

/// Component JSON (registry item)
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryItem {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub item_type: RegistryItemType,
    pub files: Vec<RegistryFile>,

    #[serde(default)]
    pub dependencies: Vec<String>,

    #[serde(default, rename = "registryDependencies")]
    pub registry_dependencies: Vec<String>,

    #[serde(default, rename = "cssVars")]
    pub css_vars: Option<CssVars>,

    #[serde(default)]
    pub css: Option<CssRules>,

    /// Deprecated Tailwind v3 config - converted to CSS for Tailwind v4
    #[serde(default)]
    pub tailwind: Option<TailwindConfig>,

    #[serde(default)]
    pub docs: Option<String>,

    #[serde(default)]
    pub categories: Vec<String>,

    #[serde(default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct CssVars {
    #[serde(default)]
    pub theme: HashMap<String, String>,
    #[serde(default)]
    pub light: HashMap<String, String>,
    #[serde(default)]
    pub dark: HashMap<String, String>,
}

/// Tailwind configuration from registry items (deprecated in Tailwind v4, but still used by many components)
/// Structure: { config: { theme: { extend: { colors, keyframes, animation } } } }
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindConfig {
    #[serde(default)]
    pub config: Option<TailwindConfigInner>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindConfigInner {
    #[serde(default)]
    pub theme: Option<TailwindTheme>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct TailwindTheme {
    #[serde(default)]
    pub extend: Option<TailwindThemeExtend>,
}

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

    /// Font family definitions: { "heading": ["Poppins", "sans-serif"] }
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

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct RegistryFile {
    pub path: String,
    pub content: String,

    /// Some registry items include "target" (often empty). Keep it optional.
    #[allow(dead_code)]
    #[serde(default)]
    pub target: Option<String>,

    #[allow(dead_code)]
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
}
