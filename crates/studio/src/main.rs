#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod registry;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::get_projects,
            commands::refresh_projects,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
