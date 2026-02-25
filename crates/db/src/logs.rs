//! Async logs database operations using `SQLx`.
//!
//! Provides [`LogsDb`] for all CRUD operations on the OTLP logs table
//! at `~/.apx/logs/db`.

use apx_common::LogRecord;
use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use tracing::debug;

/// Retention period in seconds (7 days).
const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Async logs database handle.
#[derive(Clone, Debug)]
pub struct LogsDb {
    pool: SqlitePool,
}

impl LogsDb {
    /// Open or create the database at the default location (`~/.apx/logs/db`).
    ///
    /// # Errors
    ///
    /// Returns an error if the database path cannot be determined or the database
    /// cannot be opened.
    pub async fn open() -> Result<Self, String> {
        let path = super::logs_db_path()?;
        Self::open_at(&path).await
    }

    /// Open or create the database at a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the database cannot
    /// be opened, or schema initialization fails.
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
            .map_err(|e| format!("Failed to open database: {e}"))?;

        let db = Self { pool };
        db.init_schema().await?;
        Ok(db)
    }

    /// Initialize the database schema.
    async fn init_schema(&self) -> Result<(), String> {
        sqlx::query(
            r"CREATE TABLE IF NOT EXISTS logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ns INTEGER NOT NULL,
                observed_timestamp_ns INTEGER NOT NULL,
                severity_number INTEGER,
                severity_text TEXT,
                body TEXT,
                service_name TEXT,
                app_path TEXT,
                resource_attributes TEXT,
                log_attributes TEXT,
                trace_id TEXT,
                span_id TEXT,
                created_at INTEGER DEFAULT (strftime('%s', 'now'))
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to initialize schema: {e}"))?;

        for idx_sql in [
            "CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp_ns)",
            "CREATE INDEX IF NOT EXISTS idx_logs_app_path ON logs(app_path)",
            "CREATE INDEX IF NOT EXISTS idx_logs_service ON logs(service_name)",
            "CREATE INDEX IF NOT EXISTS idx_logs_created ON logs(created_at)",
        ] {
            sqlx::query(idx_sql)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Index error: {e}"))?;
        }

        debug!("Flux storage schema initialized");
        Ok(())
    }

    /// Insert a batch of log records.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction fails or any insert fails.
    pub async fn insert_logs(&self, records: &[LogRecord]) -> Result<usize, String> {
        if records.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Transaction error: {e}"))?;

        let mut count = 0;
        for record in records {
            sqlx::query(
                r"INSERT INTO logs (
                    timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                    body, service_name, app_path, resource_attributes, log_attributes,
                    trace_id, span_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(record.timestamp_ns)
            .bind(record.observed_timestamp_ns)
            .bind(record.severity_number)
            .bind(record.severity_text.as_deref())
            .bind(record.body.as_deref())
            .bind(record.service_name.as_deref())
            .bind(record.app_path.as_deref())
            .bind(record.resource_attributes.as_deref())
            .bind(record.log_attributes.as_deref())
            .bind(record.trace_id.as_deref())
            .bind(record.span_id.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Insert error: {e}"))?;
            count += 1;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Commit error: {e}"))?;
        Ok(count)
    }

    /// Query logs for a specific app path since a given timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn query_logs(
        &self,
        app_path: Option<&str>,
        since_ns: i64,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, String> {
        let effective_ts = "COALESCE(NULLIF(timestamp_ns, 0), observed_timestamp_ns)";

        let sql = match (app_path, limit) {
            (Some(_), Some(lim)) => format!(
                r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE (app_path LIKE ?1 OR ?1 LIKE '%' || app_path || '%')
                  AND {effective_ts} >= ?2
                ORDER BY {effective_ts} ASC
                LIMIT {lim}"
            ),
            (Some(_), None) => format!(
                r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE (app_path LIKE ?1 OR ?1 LIKE '%' || app_path || '%')
                  AND {effective_ts} >= ?2
                ORDER BY {effective_ts} ASC"
            ),
            (None, Some(lim)) => format!(
                r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE {effective_ts} >= ?1
                ORDER BY {effective_ts} ASC
                LIMIT {lim}"
            ),
            (None, None) => format!(
                r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE {effective_ts} >= ?1
                ORDER BY {effective_ts} ASC"
            ),
        };

        let rows = if let Some(path) = app_path {
            let pattern = format!("%{path}%");
            sqlx::query(&sql)
                .bind(&pattern)
                .bind(since_ns)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query(&sql).bind(since_ns).fetch_all(&self.pool).await
        }
        .map_err(|e| format!("Query error: {e}"))?;

        let records = rows.iter().map(row_to_log_record).collect();
        Ok(records)
    }

    /// Get the latest log ID for change detection in follow mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn get_latest_id(&self) -> Result<i64, String> {
        let row = sqlx::query("SELECT COALESCE(MAX(id), 0) as max_id FROM logs")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| format!("Query error: {e}"))?;
        let id: i64 = row.get("max_id");
        Ok(id)
    }

    /// Query logs newer than a given ID (for follow mode).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn query_logs_after_id(
        &self,
        app_path: Option<&str>,
        after_id: i64,
    ) -> Result<Vec<LogRecord>, String> {
        let effective_ts = "COALESCE(NULLIF(timestamp_ns, 0), observed_timestamp_ns)";

        let (sql, has_app_path) = if app_path.is_some() {
            (
                format!(
                    r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                           body, service_name, app_path, resource_attributes, log_attributes,
                           trace_id, span_id
                    FROM logs
                    WHERE id > ?1 AND (app_path LIKE ?2 OR ?2 LIKE '%' || app_path || '%')
                    ORDER BY {effective_ts} ASC"
                ),
                true,
            )
        } else {
            (
                format!(
                    r"SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                           body, service_name, app_path, resource_attributes, log_attributes,
                           trace_id, span_id
                    FROM logs
                    WHERE id > ?1
                    ORDER BY {effective_ts} ASC"
                ),
                false,
            )
        };

        let rows = if has_app_path {
            let pattern = format!("%{}%", app_path.unwrap_or(""));
            sqlx::query(&sql)
                .bind(after_id)
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query(&sql).bind(after_id).fetch_all(&self.pool).await
        }
        .map_err(|e| format!("Query error: {e}"))?;

        let records = rows.iter().map(row_to_log_record).collect();
        Ok(records)
    }

    /// Delete logs older than the retention period (7 days).
    ///
    /// # Errors
    ///
    /// Returns an error if the delete query fails.
    pub async fn cleanup_old_logs(&self) -> Result<usize, String> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().cast_signed() - RETENTION_SECONDS)
            .unwrap_or(0);

        let result = sqlx::query("DELETE FROM logs WHERE created_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Delete error: {e}"))?;

        // Reason: severity_number is always 0..21 which fits in i32
        #[allow(clippy::cast_possible_truncation)]
        let deleted = result.rows_affected() as usize;
        if deleted > 0 {
            debug!("Cleaned up {} old log records", deleted);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
impl LogsDb {
    /// Get the total count of logs.
    pub async fn count_logs(&self) -> Result<i64, String> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM logs")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| format!("Query error: {e}"))?;
        let count: i64 = row.get("cnt");
        Ok(count)
    }
}

/// Map a `SQLx` row to a `LogRecord`.
fn row_to_log_record(row: &sqlx::sqlite::SqliteRow) -> LogRecord {
    LogRecord {
        timestamp_ns: row.get("timestamp_ns"),
        observed_timestamp_ns: row.get("observed_timestamp_ns"),
        severity_number: row.get("severity_number"),
        severity_text: row.get("severity_text"),
        body: row.get("body"),
        service_name: row.get("service_name"),
        app_path: row.get("app_path"),
        resource_attributes: row.get("resource_attributes"),
        log_attributes: row.get("log_attributes"),
        trace_id: row.get("trace_id"),
        span_id: row.get("span_id"),
    }
}

#[cfg(test)]
// Reason: panicking on failure is idiomatic in tests
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn temp_db() -> LogsDb {
        let dir = std::env::temp_dir().join(format!(
            "apx-db-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        LogsDb::open_at(&dir.join("test.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_create_and_insert() {
        let db = temp_db().await;

        let record = LogRecord {
            timestamp_ns: 1_234_567_890_000_000_000,
            observed_timestamp_ns: 1_234_567_890_000_000_000,
            severity_number: Some(9),
            severity_text: Some("INFO".to_string()),
            body: Some("Test log message".to_string()),
            service_name: Some("test_app".to_string()),
            app_path: Some("/tmp/test".to_string()),
            resource_attributes: None,
            log_attributes: None,
            trace_id: None,
            span_id: None,
        };

        let count = db.insert_logs(&[record]).await.unwrap();
        assert_eq!(count, 1);

        let total = db.count_logs().await.unwrap();
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn test_query() {
        let db = temp_db().await;

        let record = LogRecord {
            timestamp_ns: 1_234_567_890_000_000_000,
            observed_timestamp_ns: 1_234_567_890_000_000_000,
            severity_number: Some(9),
            severity_text: Some("INFO".to_string()),
            body: Some("Test log message".to_string()),
            service_name: Some("test_app".to_string()),
            app_path: Some("/tmp/test".to_string()),
            resource_attributes: None,
            log_attributes: None,
            trace_id: None,
            span_id: None,
        };

        db.insert_logs(&[record]).await.unwrap();

        let records = db.query_logs(Some("/tmp/test"), 0, None).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, Some("Test log message".to_string()));
    }

    #[tokio::test]
    async fn test_query_after_id() {
        let db = temp_db().await;

        let record = LogRecord {
            timestamp_ns: 1_234_567_890_000_000_000,
            observed_timestamp_ns: 1_234_567_890_000_000_000,
            severity_number: Some(9),
            severity_text: Some("INFO".to_string()),
            body: Some("First".to_string()),
            service_name: Some("test_app".to_string()),
            app_path: Some("/tmp/test".to_string()),
            resource_attributes: None,
            log_attributes: None,
            trace_id: None,
            span_id: None,
        };

        db.insert_logs(&[record]).await.unwrap();
        let id = db.get_latest_id().await.unwrap();

        let record2 = LogRecord {
            timestamp_ns: 1_234_567_891_000_000_000,
            observed_timestamp_ns: 1_234_567_891_000_000_000,
            severity_number: Some(9),
            severity_text: Some("INFO".to_string()),
            body: Some("Second".to_string()),
            service_name: Some("test_app".to_string()),
            app_path: Some("/tmp/test".to_string()),
            resource_attributes: None,
            log_attributes: None,
            trace_id: None,
            span_id: None,
        };

        db.insert_logs(&[record2]).await.unwrap();

        let records = db.query_logs_after_id(Some("/tmp/test"), id).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, Some("Second".to_string()));
    }
}
