//! Integration tests for the framework crate.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]

mod shutdown;
mod streaming;
mod supervision;
mod telemetry;

use std::path::Path;
use std::sync::Once;

// ── Python environment setup ────────────────────────────────────────────

static PYTHON_ENV_INIT: Once = Once::new();

/// Ensure `PYTHONHOME`, `VIRTUAL_ENV`, and `PYTHONPATH` are set so the
/// embedded interpreter can find its stdlib **and** venv-installed packages
/// (e.g. `uvloop`).
#[expect(unsafe_code, reason = "env::set_var required for Python interpreter")]
pub fn ensure_python_env() {
    PYTHON_ENV_INIT.call_once(|| {
        if std::env::var("PYTHONHOME").is_ok() {
            return;
        }
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
        let venv = workspace_root.join(".venv");
        let cfg_path = venv.join("pyvenv.cfg");
        let cfg = std::fs::read_to_string(&cfg_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", cfg_path.display()));

        let mut found_home = false;
        for line in cfg.lines() {
            if let Some(home_bin) = line.strip_prefix("home = ") {
                let base = Path::new(home_bin.trim()).parent().unwrap();
                unsafe {
                    std::env::set_var("PYTHONHOME", base);
                    std::env::set_var("VIRTUAL_ENV", &venv);
                }
                found_home = true;
                break;
            }
        }
        assert!(found_home, "pyvenv.cfg missing `home` key");

        // The embedded interpreter doesn't run `site.py` the same way a
        // normal `python` invocation does, so venv site-packages aren't
        // automatically added to `sys.path`. Discover and export via
        // `PYTHONPATH` so that packages like `uvloop` are importable.
        //
        // Also process `.pth` files — editable installs (e.g. `uv pip
        // install -e .`) create a `.pth` file that `site.py` would
        // normally process to add the source tree to `sys.path`.
        let lib_dir = venv.join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            for entry in entries.flatten() {
                let sp = entry.path().join("site-packages");
                if sp.is_dir() {
                    let mut paths = vec![sp.to_string_lossy().into_owned()];
                    // Read .pth files and append their entries.
                    if let Ok(pth_entries) = std::fs::read_dir(&sp) {
                        for pth in pth_entries.flatten() {
                            let p = pth.path();
                            if p.extension().is_some_and(|e| e == "pth")
                                && let Ok(content) = std::fs::read_to_string(&p)
                            {
                                for line in content.lines() {
                                    let line = line.trim();
                                    if !line.is_empty()
                                        && !line.starts_with('#')
                                        && Path::new(line).is_dir()
                                    {
                                        paths.push(line.to_owned());
                                    }
                                }
                            }
                        }
                    }
                    unsafe {
                        std::env::set_var("PYTHONPATH", paths.join(":"));
                    }
                    break;
                }
            }
        }
    });
}
