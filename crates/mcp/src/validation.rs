use std::path::PathBuf;

/// A path that has been validated as an absolute, existing directory.
///
/// Created via `try_from_str`, which validates and canonicalizes the path.
/// Derefs to `std::path::Path` for transparent use with filesystem APIs.
#[derive(Debug, Clone)]
pub struct ValidatedAppPath(PathBuf);

impl ValidatedAppPath {
    /// Validate and canonicalize an app path string.
    ///
    /// Returns `Err(rmcp::ErrorData)` with `invalid_params` code on failure,
    /// suitable for direct use with `?` in MCP handlers.
    pub fn try_from_str(s: &str) -> Result<Self, rmcp::ErrorData> {
        validate_app_path(s)
            .map(ValidatedAppPath)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))
    }
}

impl std::ops::Deref for ValidatedAppPath {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<std::path::Path> for ValidatedAppPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

/// Validate that `app_path` is an absolute path to an existing directory.
/// Returns the canonicalized path (symlinks and `..` segments resolved).
pub fn validate_app_path(app_path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(app_path);
    if !path.is_absolute() {
        return Err(format!(
            "app_path must be an absolute path, got: {app_path}"
        ));
    }
    if !path.exists() {
        return Err(format!("app_path does not exist: {app_path}"));
    }
    if !path.is_dir() {
        return Err(format!("app_path is not a directory: {app_path}"));
    }
    path.canonicalize()
        .map_err(|e| format!("Failed to canonicalize path '{app_path}': {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_relative_path() {
        let err = validate_app_path("relative/path").unwrap_err();
        assert!(err.contains("absolute path"), "got: {err}");
    }

    #[test]
    fn rejects_nonexistent_path() {
        let err = validate_app_path("/tmp/__apx_test_nonexistent_dir__").unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn rejects_file_not_dir() {
        let tmp = std::env::temp_dir().join("apx_test_validate_file");
        fs::write(&tmp, "").unwrap();
        let err = validate_app_path(tmp.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a directory"), "got: {err}");
        fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn valid_dir_returns_canonical_path() {
        let tmp = std::env::temp_dir().join("apx_test_validate_dir");
        fs::create_dir_all(&tmp).unwrap();
        let result = validate_app_path(tmp.to_str().unwrap()).unwrap();
        // canonicalize resolves symlinks, so compare canonical forms
        assert_eq!(result, tmp.canonicalize().unwrap());
        fs::remove_dir(&tmp).unwrap();
    }
}
