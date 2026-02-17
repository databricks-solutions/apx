//! Common utilities for the SQLite FTS5 search index.
//!
//! Provides a global SQLite connection singleton at `~/.apx/search.db` with WAL mode.
//! All search operations (component index, SDK docs index) share this single connection.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Global SQLite connection singleton
static SEARCH_DB: OnceLock<Arc<Mutex<Connection>>> = OnceLock::new();

/// Get the default search database path (~/.apx/search.db)
fn default_db_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Could not determine home directory".to_string())?;
    Ok(home.join(".apx").join("search.db"))
}

/// Open a SQLite connection with WAL mode and recommended pragmas.
fn open_connection(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create database directory: {e}"))?;
    }

    let conn =
        Connection::open(path).map_err(|e| format!("Failed to open search database: {e}"))?;

    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| format!("Failed to set pragmas: {e}"))?;

    Ok(conn)
}

/// Get or initialize the global search database connection at `~/.apx/search.db`.
pub fn get_connection() -> Result<Arc<Mutex<Connection>>, String> {
    if let Some(conn) = SEARCH_DB.get() {
        return Ok(Arc::clone(conn));
    }

    let path = default_db_path()?;
    let conn = open_connection(&path)?;
    let arc = Arc::new(Mutex::new(conn));

    // Another thread may have initialized it; that's fine, we just use theirs
    Ok(Arc::clone(SEARCH_DB.get_or_init(|| arc)))
}

/// Get a search database connection at a specific path (for testing).
pub fn get_connection_at(path: &Path) -> Result<Arc<Mutex<Connection>>, String> {
    let conn = open_connection(path)?;
    Ok(Arc::new(Mutex::new(conn)))
}

/// Check if a table exists in the database.
pub fn table_exists(conn: &Connection, table_name: &str) -> Result<bool, String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table_name],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to check table existence: {e}"))?;
    Ok(exists)
}

/// Check if the legacy LanceDB directory exists and log a warning.
pub fn check_legacy_lancedb() {
    if let Some(home) = dirs::home_dir() {
        let legacy_path = home.join(".apx").join("db");
        if legacy_path.is_dir() {
            tracing::warn!(
                "Legacy LanceDB directory found at {}. It is no longer used. \
                 Remove it with: rm -rf {}",
                legacy_path.display(),
                legacy_path.display()
            );
        }
    }
}

/// Sanitize a query string for FTS5 MATCH syntax.
/// Wraps each whitespace-separated term in double quotes for safe literal matching.
pub fn sanitize_fts5_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            // Strip any existing quotes and special FTS5 operators
            let clean: String = term
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            if clean.is_empty() {
                String::new()
            } else {
                format!("\"{clean}\"")
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_fts5_query_basic() {
        assert_eq!(sanitize_fts5_query("hello world"), "\"hello\" \"world\"");
    }

    #[test]
    fn test_sanitize_fts5_query_special_chars() {
        assert_eq!(
            sanitize_fts5_query("hello* OR world"),
            "\"hello\" \"OR\" \"world\""
        );
    }

    #[test]
    fn test_sanitize_fts5_query_empty() {
        assert_eq!(sanitize_fts5_query(""), "");
        assert_eq!(sanitize_fts5_query("   "), "");
    }

    #[test]
    fn test_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test_table (id INTEGER)")
            .unwrap();

        assert!(table_exists(&conn, "test_table").unwrap());
        assert!(!table_exists(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn test_get_connection_at() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test_search.db");
        let conn = get_connection_at(&path).unwrap();

        // Verify connection works
        let guard = conn.lock().unwrap();
        guard
            .execute_batch("CREATE TABLE test (id INTEGER)")
            .unwrap();
    }
}
