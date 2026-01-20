use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Subset of components.json we actually need (plus "style")
#[derive(Debug, Deserialize, Clone)]
pub struct ComponentsJson {
    #[serde(default)]
    pub style: Option<String>,

    #[serde(default)]
    pub aliases: HashMap<String, String>,

    /// Registries can be:
    ///   - string template: "https://example.com/r/{name}.json"
    ///   - advanced object: { "url": "...", "headers": {...}, "params": {...} }
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,

    pub tailwind: TailwindConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TailwindConfig {
    pub css: String,
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
