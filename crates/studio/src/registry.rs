use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const REGISTRY_FILENAME: &str = "registry.toml";
const APX_HOME_DIR: &str = ".apx";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryFile {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerEntry>,
}

pub fn registry_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Failed to determine home directory")?;
    Ok(home.join(APX_HOME_DIR).join(REGISTRY_FILENAME))
}

pub fn load() -> Result<RegistryFile, String> {
    let path = registry_path()?;
    if path.exists() {
        let contents =
            fs::read_to_string(&path).map_err(|e| format!("Failed to read registry: {e}"))?;
        toml::from_str(&contents).map_err(|e| format!("Failed to parse registry: {e}"))
    } else {
        Ok(RegistryFile::default())
    }
}
