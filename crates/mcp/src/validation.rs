use std::path::PathBuf;

/// Validate that `app_path` is an absolute path to an existing directory.
///
/// Returns the canonicalized `PathBuf` on success, or `rmcp::ErrorData`
/// with `invalid_params` code on failure — suitable for direct `?` in MCP handlers.
pub fn validated_app_path(s: &str) -> Result<PathBuf, rmcp::ErrorData> {
    let path = PathBuf::from(s);
    if !path.is_absolute() {
        return Err(rmcp::ErrorData::invalid_params(
            format!("app_path must be an absolute path, got: {s}"),
            None,
        ));
    }
    if !path.exists() {
        return Err(rmcp::ErrorData::invalid_params(
            format!("app_path does not exist: {s}"),
            None,
        ));
    }
    if !path.is_dir() {
        return Err(rmcp::ErrorData::invalid_params(
            format!("app_path is not a directory: {s}"),
            None,
        ));
    }
    path.canonicalize().map_err(|e| {
        rmcp::ErrorData::invalid_params(format!("Failed to canonicalize path '{s}': {e}"), None)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_relative_path() {
        let err = validated_app_path("relative/path").unwrap_err();
        assert!(
            err.message.contains("absolute path"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn rejects_nonexistent_path() {
        let err = validated_app_path("/tmp/__apx_test_nonexistent_dir__").unwrap_err();
        assert!(
            err.message.contains("does not exist"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn rejects_file_not_dir() {
        let tmp = std::env::temp_dir().join("apx_test_validate_file");
        fs::write(&tmp, "").unwrap();
        let err = validated_app_path(tmp.to_str().unwrap()).unwrap_err();
        assert!(
            err.message.contains("not a directory"),
            "got: {}",
            err.message
        );
        fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn valid_dir_returns_canonical_path() {
        let tmp = std::env::temp_dir().join("apx_test_validate_dir");
        fs::create_dir_all(&tmp).unwrap();
        let result = validated_app_path(tmp.to_str().unwrap()).unwrap();
        assert_eq!(result, tmp.canonicalize().unwrap());
        fs::remove_dir(&tmp).unwrap();
    }
}
