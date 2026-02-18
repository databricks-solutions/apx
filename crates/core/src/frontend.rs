use std::path::{Path, PathBuf};

use crate::common::read_project_metadata;
use crate::interop::ensure_frontend_entrypoint;

/// Prepare arguments for running the frontend entrypoint
/// Returns (entrypoint_path, args, app_name) where args are [mode, ui_root, out_dir, public_dir]
pub fn prepare_frontend_args(
    app_dir: &Path,
    mode: &str,
) -> Result<(PathBuf, Vec<String>, String), String> {
    // 1. Read project metadata from pyproject.toml
    let metadata = read_project_metadata(app_dir)?;

    // 2. Resolve all paths to absolute
    let ui_root = metadata
        .ui_root
        .as_ref()
        .ok_or("Project has no UI configured (missing [tool.apx.ui] in pyproject.toml)")?;
    let ui_root_abs = app_dir.join(ui_root);
    let out_dir_abs = metadata.dist_dir(app_dir);
    let public_dir_abs = ui_root_abs.join("public");

    // Note: __dist__ directory is created by write_metadata_file()

    // 3. Write entrypoint.ts into project's node_modules/.apx/
    let entrypoint = ensure_frontend_entrypoint(app_dir)?;

    // 4. Prepare arguments
    let args = vec![
        mode.to_string(),
        ui_root_abs.to_string_lossy().to_string(),
        out_dir_abs.to_string_lossy().to_string(),
        public_dir_abs.to_string_lossy().to_string(),
    ];

    Ok((entrypoint, args, metadata.app_name))
}
