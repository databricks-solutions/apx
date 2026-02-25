//! APX Studio - Convenient and simple UI to develop with `apx` framework.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod registry;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::get_projects,
            commands::refresh_projects,
        ])
        .run(tauri::generate_context!())
        .map_err(|e| format!("error while running tauri application: {e}"))?;

    Ok(())
}
