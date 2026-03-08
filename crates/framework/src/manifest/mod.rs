//! Manifest I/O and validation.
//!
//! Provides [`load`] / [`save`] for reading and writing [`AppManifest`] JSON
//! files, plus [`check_version`] for detecting stale manifests.

pub mod validate;

use crate::route::{AppManifest, ManifestMeta};
use std::path::Path;

/// Manifest-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Filesystem I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON deserialization failure.
    #[error("failed to deserialize manifest: {0}")]
    Deserialize(#[from] serde_json::Error),
    /// The manifest was built by a different apx version.
    #[error("manifest version mismatch: built with {manifest}, running {running}")]
    VersionMismatch {
        /// Version recorded in the manifest.
        manifest: String,
        /// Version of the currently running apx binary.
        running: String,
    },
    /// Manifest is missing required build metadata (`meta` field).
    #[error("manifest has no build metadata — was it produced by `apx build`?")]
    MissingMeta,
}

/// Load an [`AppManifest`] from a JSON file.
pub fn load(path: &Path) -> Result<AppManifest, ManifestError> {
    let bytes = std::fs::read(path)?;
    let manifest: AppManifest = serde_json::from_slice(&bytes)?;
    Ok(manifest)
}

/// Save an [`AppManifest`] to a JSON file (pretty-printed).
pub fn save(manifest: &AppManifest, path: &Path) -> Result<(), ManifestError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(path, json.as_bytes())?;
    Ok(())
}

/// Check that the manifest was built by the same apx version.
pub fn check_version(manifest: &AppManifest) -> Result<(), ManifestError> {
    let running = env!("CARGO_PKG_VERSION");
    let Some(meta) = &manifest.meta else {
        return Ok(());
    };
    if meta.apx_version != running {
        return Err(ManifestError::VersionMismatch {
            manifest: meta.apx_version.clone(),
            running: running.to_owned(),
        });
    }
    Ok(())
}

/// Validate that a manifest is suitable for production serving.
///
/// Checks that:
/// - `meta` is present (manifest was produced by `apx build`)
/// - The apx version matches the running binary
///
/// Returns the [`ManifestMeta`] on success.
pub fn validate_for_serving(manifest: &AppManifest) -> Result<&ManifestMeta, ManifestError> {
    let meta = manifest.meta.as_ref().ok_or(ManifestError::MissingMeta)?;
    check_version(manifest)?;
    Ok(meta)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use crate::route::{AppManifest, AppModule, BodyLimit, ManifestMeta};
    use std::io;

    fn minimal_manifest() -> AppManifest {
        AppManifest {
            meta: None,
            routes: Vec::new(),
            dependency_graph: Vec::new(),
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
        }
    }

    fn manifest_with_version(version: &str) -> AppManifest {
        let mut m = minimal_manifest();
        m.meta = Some(ManifestMeta {
            apx_version: version.to_owned(),
            python_version: "3.12.0".to_owned(),
            fastapi_version: None,
            build_timestamp: "2025-01-01T00:00:00Z".to_owned(),
            app_module: AppModule::new("backend.app").unwrap(),
            source_hash: None,
        });
        m
    }

    #[test]
    fn load_and_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let original = minimal_manifest();

        save(&original, &path).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(original.routes.len(), loaded.routes.len());
        assert_eq!(original.max_body_limit, loaded.max_body_limit);
    }

    #[test]
    fn load_nonexistent_file() {
        let result = load(Path::new("/tmp/nonexistent_manifest_12345.json"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ManifestError::Io(_)));
    }

    #[test]
    fn load_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"not json").unwrap();

        let result = load(&path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ManifestError::Deserialize(_)));
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("manifest.json");
        let manifest = minimal_manifest();

        save(&manifest, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn check_version_match() {
        let m = manifest_with_version(env!("CARGO_PKG_VERSION"));
        assert!(check_version(&m).is_ok());
    }

    #[test]
    fn check_version_mismatch() {
        let m = manifest_with_version("0.0.0-fake");
        let err = check_version(&m).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("0.0.0-fake"),
            "should contain manifest version"
        );
        assert!(
            msg.contains(env!("CARGO_PKG_VERSION")),
            "should contain running version"
        );
    }

    #[test]
    fn check_version_no_meta() {
        let m = minimal_manifest();
        assert!(check_version(&m).is_ok());
    }

    #[test]
    fn manifest_error_display_io() {
        let err = ManifestError::Io(io::Error::new(io::ErrorKind::NotFound, "gone"));
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn manifest_error_display_missing_meta() {
        let err = ManifestError::MissingMeta;
        let msg = err.to_string();
        assert!(msg.contains("no build metadata"));
    }

    #[test]
    fn validate_for_serving_with_meta() {
        let m = manifest_with_version(env!("CARGO_PKG_VERSION"));
        let meta = validate_for_serving(&m).unwrap();
        assert_eq!(meta.apx_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn validate_for_serving_missing_meta() {
        let m = minimal_manifest();
        let err = validate_for_serving(&m).unwrap_err();
        assert!(matches!(err, ManifestError::MissingMeta));
    }

    #[test]
    fn validate_for_serving_version_mismatch() {
        let m = manifest_with_version("0.0.0-fake");
        let err = validate_for_serving(&m).unwrap_err();
        assert!(matches!(err, ManifestError::VersionMismatch { .. }));
    }
}
