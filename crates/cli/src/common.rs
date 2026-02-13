//! Common types shared across CLI commands

use clap::ValueEnum;

/// Project template types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Template {
    /// Minimal template with basic UI structure
    Minimal,
    /// Standard template with UI and API
    Essential,
    /// Template with database integration
    Stateful,
}

impl Template {
    /// Get the directory name for this template addon
    pub fn directory_name(&self) -> &str {
        match self {
            Template::Minimal => "minimal-ui",
            Template::Essential => "base",
            Template::Stateful => "stateful",
        }
    }
}

/// AI assistant configuration types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Assistant {
    /// Cursor IDE rules and MCP config
    Cursor,
    /// VSCode instructions and MCP config
    Vscode,
    /// OpenAI Codex AGENTS.md file
    Codex,
    /// Claude project file and MCP config
    Claude,
}

impl Assistant {
    /// Get the directory name for this assistant addon
    pub fn directory_name(&self) -> &str {
        match self {
            Assistant::Cursor => "cursor",
            Assistant::Vscode => "vscode",
            Assistant::Codex => "codex",
            Assistant::Claude => "claude",
        }
    }
}

/// UI layout types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Layout {
    /// Basic layout without sidebar
    Basic,
    /// Sidebar navigation layout
    Sidebar,
}

impl Layout {
    /// Get the directory name for this layout addon (None for Basic)
    pub fn directory_name(&self) -> Option<&str> {
        match self {
            Layout::Basic => None,
            Layout::Sidebar => Some("sidebar"),
        }
    }
}
