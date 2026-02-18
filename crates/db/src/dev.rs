//! Async dev database operations using SQLx.
//!
//! Provides [`DevDb`] as the connection pool for the dev database at `~/.apx/dev/db`.
//! This database holds search indexes (FTS5) and will hold future dev-related tables.

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;

/// Async dev database handle.
#[derive(Clone, Debug)]
pub struct DevDb {
    pool: SqlitePool,
}

impl DevDb {
    /// Open or create the dev database at the default location (`~/.apx/dev/db`).
    pub async fn open() -> Result<Self, String> {
        let path = super::dev_db_path()?;
        Self::open_at(&path).await
    }

    /// Open or create the dev database at a specific path.
    pub async fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create database directory: {e}"))?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .map_err(|e| format!("Failed to open dev database: {e}"))?;

        Ok(Self { pool })
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Check if a table exists in the database.
pub async fn table_exists(pool: &SqlitePool, table_name: &str) -> Result<bool, String> {
    let row: (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)")
            .bind(table_name)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to check table existence: {e}"))?;
    Ok(row.0)
}

/// Sanitize a query string for FTS5 MATCH syntax.
/// Wraps each whitespace-separated term in double quotes for safe literal matching.
/// Terms are joined with OR so that partial matches are returned — FTS5 ranking
/// naturally scores documents with more matching terms higher.
pub fn sanitize_fts5_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
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
        .join(" OR ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_fts5_query_basic() {
        assert_eq!(sanitize_fts5_query("hello world"), "\"hello\" OR \"world\"");
    }

    #[test]
    fn test_sanitize_fts5_query_special_chars() {
        assert_eq!(
            sanitize_fts5_query("hello* OR world"),
            "\"hello\" OR \"OR\" OR \"world\""
        );
    }

    #[test]
    fn test_sanitize_fts5_query_empty() {
        assert_eq!(sanitize_fts5_query(""), "");
        assert_eq!(sanitize_fts5_query("   "), "");
    }

    #[tokio::test]
    async fn test_table_exists() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE test_table (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();

        assert!(table_exists(&pool, "test_table").await.unwrap());
        assert!(!table_exists(&pool, "nonexistent").await.unwrap());
    }

    #[tokio::test]
    async fn test_dev_db_open() {
        let dir = std::env::temp_dir().join(format!(
            "apx-dev-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = DevDb::open_at(&dir.join("test.db")).await.unwrap();

        // Verify the pool works
        sqlx::query("CREATE TABLE test (id INTEGER)")
            .execute(db.pool())
            .await
            .unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
