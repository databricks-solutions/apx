use std::path::Path;

/// Format a path as relative to the app directory, with ./ prefix and cleaned up ././ patterns.
pub fn format_relative_path(path: &Path, app_dir: &Path) -> String {
    path.strip_prefix(app_dir)
        .map_or_else(|_| path.display().to_string(), format_relative_string)
}

/// Format a path as a clean relative string, stripping a leading `./` if present.
pub fn format_relative_string(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    // Remove leading ./ if present
    s.strip_prefix("./").unwrap_or(&s).to_string()
}
