use std::path::PathBuf;
use std::sync::OnceLock;

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_app_dir(app_dir: PathBuf) -> Result<(), String> {
    if let Some(existing) = APP_DIR.get() {
        if existing != &app_dir {
            return Err(format!(
                "App directory already set to {}",
                existing.display()
            ));
        }
        return Ok(());
    }
    APP_DIR
        .set(app_dir)
        .map_err(|_| "Failed to set app directory".to_string())
}

pub fn get_app_dir() -> Option<PathBuf> {
    APP_DIR.get().cloned()
}
