#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]
#![deny(warnings, unused_must_use, dead_code, missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

mod commands;
mod registry;

#[allow(clippy::expect_used)]
fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::get_projects,
            commands::refresh_projects,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
