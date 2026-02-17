//! Common utilities for the search index.

/// Check if the legacy LanceDB directory or old search.db exists and log a warning.
pub fn check_legacy_paths() {
    if let Some(home) = dirs::home_dir() {
        let legacy_lancedb = home.join(".apx").join("db");
        if legacy_lancedb.is_dir() {
            tracing::warn!(
                "Legacy LanceDB directory found at {}. It is no longer used. \
                 Remove it with: rm -rf {}",
                legacy_lancedb.display(),
                legacy_lancedb.display()
            );
        }

        let legacy_search_db = home.join(".apx").join("search.db");
        if legacy_search_db.exists() {
            tracing::warn!(
                "Legacy search database found at {}. Search now uses ~/.apx/dev/db. \
                 Remove it with: rm {}",
                legacy_search_db.display(),
                legacy_search_db.display()
            );
        }
    }
}
