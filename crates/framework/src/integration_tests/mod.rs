//! Integration tests for the framework crate.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]

mod supervision;
mod telemetry;

use std::path::Path;
use std::sync::Once;

// ── Python environment setup ────────────────────────────────────────────

static PYTHON_ENV_INIT: Once = Once::new();

/// Ensure `PYTHONHOME` and `VIRTUAL_ENV` are set so the embedded interpreter
/// can find its stdlib.
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
    });
}
