//! Compile a framework manifest from a FastAPI app.
//!
//! Stubbed during moonshot Step 0 — manifest compilation depends on deleted
//! `manifest` and `route` modules. Will be restored when app_loader lands.

/// Compile and write a manifest for the given app module.
///
/// Currently stubbed — returns an error indicating manifest compilation
/// is not yet available in the new architecture.
#[expect(
    clippy::unused_async,
    reason = "async signature kept for API compatibility"
)]
pub async fn compile_manifest(
    _app_path: &std::path::Path,
    _build_dir: &std::path::Path,
    _app_module: &str,
) -> Result<(), String> {
    Err("manifest compilation is temporarily unavailable (moonshot Step 0)".to_owned())
}
