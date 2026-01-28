//! SQLite storage for flux OTEL logs.
//!
//! This module handles all database operations for storing and retrieving
//! OpenTelemetry logs in a local SQLite database.

use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Directory for flux data (~/.apx/logs)
const FLUX_DIR: &str = ".apx/logs";

/// Database filename
const DB_FILENAME: &str = "db";

/// Retention period in seconds (7 days)
const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A log record to be inserted into the database.
#[derive(Debug, Clone)]
pub struct LogRecord {
    pub timestamp_ns: i64,
    pub observed_timestamp_ns: i64,
    pub severity_number: Option<i32>,
    pub severity_text: Option<String>,
    pub body: Option<String>,
    pub service_name: Option<String>,
    pub app_path: Option<String>,
    pub resource_attributes: Option<String>, // JSON
    pub log_attributes: Option<String>,      // JSON
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

/// Thread-safe storage handle.
#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    /// Open or create the database at the default location (~/.apx/logs/db).
    pub fn open() -> Result<Self, String> {
        let path = db_path()?;
        Self::open_at(&path)
    }

    /// Open or create the database at a specific path.
    pub fn open_at(path: &Path) -> Result<Self, String> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create database directory: {}", e))?;
        }

        let conn = Connection::open(path)
            .map_err(|e| format!("Failed to open database: {}", e))?;

        // Enable WAL mode for better concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to set pragmas: {}", e))?;

        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        storage.init_schema()?;
        Ok(storage)
    }

    /// Initialize the database schema.
    fn init_schema(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS logs (
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
            );

            CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_logs_app_path ON logs(app_path);
            CREATE INDEX IF NOT EXISTS idx_logs_service ON logs(service_name);
            CREATE INDEX IF NOT EXISTS idx_logs_created ON logs(created_at);
            "#,
        )
        .map_err(|e| format!("Failed to initialize schema: {}", e))?;

        debug!("Flux storage schema initialized");
        Ok(())
    }

    /// Insert a batch of log records.
    pub fn insert_logs(&self, records: &[LogRecord]) -> Result<usize, String> {
        if records.is_empty() {
            return Ok(0);
        }

        let mut conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;
        let tx = conn.transaction().map_err(|e| format!("Transaction error: {}", e))?;

        let mut count = 0;
        {
            let mut stmt = tx
                .prepare_cached(
                    r#"
                    INSERT INTO logs (
                        timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                        body, service_name, app_path, resource_attributes, log_attributes,
                        trace_id, span_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    "#,
                )
                .map_err(|e| format!("Prepare error: {}", e))?;

            for record in records {
                stmt.execute(params![
                    record.timestamp_ns,
                    record.observed_timestamp_ns,
                    record.severity_number,
                    record.severity_text,
                    record.body,
                    record.service_name,
                    record.app_path,
                    record.resource_attributes,
                    record.log_attributes,
                    record.trace_id,
                    record.span_id,
                ])
                .map_err(|e| format!("Insert error: {}", e))?;
                count += 1;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {}", e))?;
        Ok(count)
    }

    /// Query logs for a specific app path since a given timestamp.
    /// Uses COALESCE to fall back to observed_timestamp_ns when timestamp_ns is 0,
    /// which happens with OpenTelemetry tracing bridge logs.
    pub fn query_logs(
        &self,
        app_path: Option<&str>,
        since_ns: i64,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        // Use effective_ts to handle logs where timestamp_ns is 0 (e.g., APX internal logs)
        const EFFECTIVE_TS: &str = "COALESCE(NULLIF(timestamp_ns, 0), observed_timestamp_ns)";

        let sql = match (app_path, limit) {
            (Some(_), Some(lim)) => format!(
                r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE (app_path LIKE ?1 OR ?1 LIKE '%' || app_path || '%')
                  AND {EFFECTIVE_TS} >= ?2
                ORDER BY {EFFECTIVE_TS} ASC
                LIMIT {}
                "#,
                lim
            ),
            (Some(_), None) => format!(
                r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE (app_path LIKE ?1 OR ?1 LIKE '%' || app_path || '%')
                  AND {EFFECTIVE_TS} >= ?2
                ORDER BY {EFFECTIVE_TS} ASC
                "#
            ),
            (None, Some(lim)) => format!(
                r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE {EFFECTIVE_TS} >= ?1
                ORDER BY {EFFECTIVE_TS} ASC
                LIMIT {}
                "#,
                lim
            ),
            (None, None) => format!(
                r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE {EFFECTIVE_TS} >= ?1
                ORDER BY {EFFECTIVE_TS} ASC
                "#
            ),
        };

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("Prepare error: {}", e))?;

        let rows = if let Some(path) = app_path {
            let pattern = format!("%{}%", path);
            stmt.query_map(params![pattern, since_ns], map_row)
                .map_err(|e| format!("Query error: {}", e))?
        } else {
            stmt.query_map(params![since_ns], map_row)
                .map_err(|e| format!("Query error: {}", e))?
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|e| format!("Row error: {}", e))?);
        }

        Ok(records)
    }

    /// Get the latest log ID for change detection in follow mode.
    pub fn get_latest_id(&self) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;
        let id: i64 = conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM logs", [], |row| row.get(0))
            .map_err(|e| format!("Query error: {}", e))?;
        Ok(id)
    }

    /// Query logs newer than a given ID (for follow mode).
    /// Uses COALESCE for ordering to handle logs where timestamp_ns is 0.
    pub fn query_logs_after_id(
        &self,
        app_path: Option<&str>,
        after_id: i64,
    ) -> Result<Vec<LogRecord>, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        // Use effective_ts to handle logs where timestamp_ns is 0 (e.g., APX internal logs)
        const EFFECTIVE_TS: &str = "COALESCE(NULLIF(timestamp_ns, 0), observed_timestamp_ns)";

        let (sql, needs_app_path) = if let Some(_) = app_path {
            (
                format!(
                    r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE id > ?1 AND (app_path LIKE ?2 OR ?2 LIKE '%' || app_path || '%')
                ORDER BY {EFFECTIVE_TS} ASC
                "#
                ),
                true,
            )
        } else {
            (
                format!(
                    r#"
                SELECT timestamp_ns, observed_timestamp_ns, severity_number, severity_text,
                       body, service_name, app_path, resource_attributes, log_attributes,
                       trace_id, span_id
                FROM logs
                WHERE id > ?1
                ORDER BY {EFFECTIVE_TS} ASC
                "#
                ),
                false,
            )
        };

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("Prepare error: {}", e))?;

        let rows = if needs_app_path {
            let pattern = format!("%{}%", app_path.unwrap());
            stmt.query_map(params![after_id, pattern], map_row)
                .map_err(|e| format!("Query error: {}", e))?
        } else {
            stmt.query_map(params![after_id], map_row)
                .map_err(|e| format!("Query error: {}", e))?
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|e| format!("Row error: {}", e))?);
        }

        Ok(records)
    }

    /// Delete logs older than the retention period (7 days).
    pub fn cleanup_old_logs(&self) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;

        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64 - RETENTION_SECONDS)
            .unwrap_or(0);

        let deleted = conn
            .execute("DELETE FROM logs WHERE created_at < ?1", params![cutoff])
            .map_err(|e| format!("Delete error: {}", e))?;

        if deleted > 0 {
            debug!("Cleaned up {} old log records", deleted);
        }

        Ok(deleted)
    }

    /// Get the total count of logs.
    #[allow(dead_code)]
    pub fn count_logs(&self) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {}", e))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
            .map_err(|e| format!("Query error: {}", e))?;
        Ok(count)
    }
}

/// Map a database row to a LogRecord.
fn map_row(row: &rusqlite::Row) -> rusqlite::Result<LogRecord> {
    Ok(LogRecord {
        timestamp_ns: row.get(0)?,
        observed_timestamp_ns: row.get(1)?,
        severity_number: row.get(2)?,
        severity_text: row.get(3)?,
        body: row.get(4)?,
        service_name: row.get(5)?,
        app_path: row.get(6)?,
        resource_attributes: row.get(7)?,
        log_attributes: row.get(8)?,
        trace_id: row.get(9)?,
        span_id: row.get(10)?,
    })
}

/// Get the flux directory path (~/.apx/logs).
pub fn flux_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(FLUX_DIR))
}

/// Get the database path (~/.apx/logs/db).
pub fn db_path() -> Result<PathBuf, String> {
    Ok(flux_dir()?.join(DB_FILENAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_storage_create_and_insert() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::open_at(&db_path).unwrap();

        let record = LogRecord {
            timestamp_ns: 1234567890000000000,
            observed_timestamp_ns: 1234567890000000000,
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

        let count = storage.insert_logs(&[record]).unwrap();
        assert_eq!(count, 1);

        let total = storage.count_logs().unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_storage_query() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let storage = Storage::open_at(&db_path).unwrap();

        let record = LogRecord {
            timestamp_ns: 1234567890000000000,
            observed_timestamp_ns: 1234567890000000000,
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

        storage.insert_logs(&[record]).unwrap();

        let records = storage.query_logs(Some("/tmp/test"), 0, None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, Some("Test log message".to_string()));
    }
}
