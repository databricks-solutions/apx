use crate::registry;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub port: u16,
    pub exists: bool,
}

#[tauri::command]
pub fn get_projects() -> Result<Vec<ProjectInfo>, String> {
    let registry = registry::load()?;
    let projects = registry
        .servers
        .into_iter()
        .map(|(path, entry)| {
            let name = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let exists = Path::new(&path).exists();
            ProjectInfo {
                path,
                name,
                port: entry.port,
                exists,
            }
        })
        .collect();
    Ok(projects)
}

#[tauri::command]
pub fn refresh_projects() -> Result<Vec<ProjectInfo>, String> {
    get_projects()
}
