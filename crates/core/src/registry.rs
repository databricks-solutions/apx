//! Global application registry for tracking project-to-port mappings.
//!
//! The registry is stored at `~/.apx/registry.toml` and persists port allocations
//! across server restarts. Ports are assigned incrementally starting from 9000
//! and remain associated with a project until its directory is deleted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::dev::common::DEV_PORT_START;

/// Registry filename
const REGISTRY_FILENAME: &str = "registry.toml";
/// APX directory name in home
const APX_HOME_DIR: &str = ".apx";

/// Server entry in the registry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerEntry {
    pub port: u16,
}

/// Registry file structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryFile {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerEntry>,
}

/// Application registry for managing project-to-port mappings
#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    data: RegistryFile,
}

impl Registry {
    /// Get the path to the registry file (~/.apx/registry.toml)
    pub fn registry_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir().ok_or("Failed to determine home directory")?;
        Ok(home.join(APX_HOME_DIR).join(REGISTRY_FILENAME))
    }

    /// Load the registry from disk, or create an empty one if it doesn't exist
    pub fn load() -> Result<Self, String> {
        let path = Self::registry_path()?;

        let data = if path.exists() {
            let contents = fs::read_to_string(&path)
                .map_err(|err| format!("Failed to read registry file: {err}"))?;
            toml::from_str(&contents)
                .map_err(|err| format!("Failed to parse registry file: {err}"))?
        } else {
            RegistryFile::default()
        };

        Ok(Self { path, data })
    }

    /// Save the registry to disk with clean formatting:
    /// ```toml
    /// [servers]
    /// "/path/to/project" = { port = 9000 }
    /// ```
    pub fn save(&self) -> Result<(), String> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create registry directory: {err}"))?;
        }

        // Build document with inline tables for cleaner formatting
        let mut doc = DocumentMut::new();
        let mut servers_table = Table::new();

        for (path, entry) in &self.data.servers {
            let mut inline = InlineTable::new();
            inline.insert("port", Value::from(i64::from(entry.port)));
            servers_table.insert(path, Item::Value(Value::InlineTable(inline)));
        }

        doc.insert("servers", Item::Table(servers_table));

        fs::write(&self.path, doc.to_string())
            .map_err(|err| format!("Failed to write registry file: {err}"))
    }

    /// Remove entries where the project directory no longer exists
    pub fn cleanup_stale_entries(&mut self) -> Vec<String> {
        let stale_paths: Vec<String> = self
            .data
            .servers
            .keys()
            .filter(|path| !Path::new(path).exists())
            .cloned()
            .collect();

        for path in &stale_paths {
            self.data.servers.remove(path);
        }

        stale_paths
    }

    /// Get the port for a project, or allocate a new one if not registered.
    ///
    /// If `preferred_port` is provided and different from the currently registered port,
    /// the registry will be updated to use the preferred port.
    pub fn get_or_allocate_port(
        &mut self,
        project_path: &Path,
        preferred_port: Option<u16>,
    ) -> Result<u16, String> {
        let canonical_path = project_path
            .canonicalize()
            .map_err(|err| format!("Failed to canonicalize project path: {err}"))?;
        let path_str = canonical_path.display().to_string();

        // If preferred port is specified, use it and update registry
        if let Some(port) = preferred_port {
            self.data.servers.insert(path_str, ServerEntry { port });
            return Ok(port);
        }

        // Check if project already has a port assigned
        if let Some(entry) = self.data.servers.get(&path_str) {
            return Ok(entry.port);
        }

        // Allocate the next available port
        let port = self.allocate_next_port()?;
        self.data.servers.insert(path_str, ServerEntry { port });
        Ok(port)
    }

    /// Find the next available port starting from DEV_PORT_START (9000)
    fn allocate_next_port(&self) -> Result<u16, String> {
        let used_ports: std::collections::HashSet<u16> =
            self.data.servers.values().map(|e| e.port).collect();

        for port in DEV_PORT_START..=u16::MAX {
            if !used_ports.contains(&port) {
                return Ok(port);
            }
        }

        Err("No available ports".to_string())
    }

    /// Get the number of registered servers
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.data.servers.len()
    }

    /// Check if the registry is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.data.servers.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_registry(temp_dir: &TempDir) -> Registry {
        let path = temp_dir.path().join("registry.toml");
        Registry {
            path,
            data: RegistryFile::default(),
        }
    }

    #[test]
    fn test_allocate_next_port_empty() {
        let temp_dir = TempDir::new().unwrap();
        let registry = create_test_registry(&temp_dir);
        assert_eq!(registry.allocate_next_port().unwrap(), DEV_PORT_START);
    }

    #[test]
    fn test_allocate_next_port_incremental() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = create_test_registry(&temp_dir);

        // Add some ports
        registry.data.servers.insert(
            "/project1".to_string(),
            ServerEntry {
                port: DEV_PORT_START,
            },
        );
        registry.data.servers.insert(
            "/project2".to_string(),
            ServerEntry {
                port: DEV_PORT_START + 1,
            },
        );

        // Next should be DEV_PORT_START + 2
        assert_eq!(registry.allocate_next_port().unwrap(), DEV_PORT_START + 2);
    }

    #[test]
    fn test_allocate_next_port_fills_gaps() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = create_test_registry(&temp_dir);

        // Add ports with a gap
        registry.data.servers.insert(
            "/project1".to_string(),
            ServerEntry {
                port: DEV_PORT_START,
            },
        );
        registry.data.servers.insert(
            "/project2".to_string(),
            ServerEntry {
                port: DEV_PORT_START + 2,
            },
        );

        // Should fill the gap at DEV_PORT_START + 1
        assert_eq!(registry.allocate_next_port().unwrap(), DEV_PORT_START + 1);
    }

    #[test]
    fn test_cleanup_stale_entries() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = create_test_registry(&temp_dir);

        // Add an existing path and a non-existing path
        let existing_path = temp_dir.path().to_string_lossy().to_string();
        registry.data.servers.insert(
            existing_path.clone(),
            ServerEntry {
                port: DEV_PORT_START,
            },
        );
        registry.data.servers.insert(
            "/non/existing/path".to_string(),
            ServerEntry {
                port: DEV_PORT_START + 1,
            },
        );

        let removed = registry.cleanup_stale_entries();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], "/non/existing/path");
        assert!(registry.data.servers.contains_key(&existing_path));
        assert!(!registry.data.servers.contains_key("/non/existing/path"));
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("registry.toml");

        // Create and save
        let mut registry = Registry {
            path: path.clone(),
            data: RegistryFile::default(),
        };
        registry
            .data
            .servers
            .insert("/test/project".to_string(), ServerEntry { port: 9001 });
        registry.save().unwrap();

        // Load and verify
        let contents = fs::read_to_string(&path).unwrap();
        let loaded: RegistryFile = toml::from_str(&contents).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.servers.get("/test/project").unwrap().port, 9001);
    }
}
