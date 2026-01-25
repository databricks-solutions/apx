//! Common utilities for working with LanceDB indices
//!
//! This module provides thread-safe access to LanceDB with a global shared connection
//! and a RwLock for write operations. Reads can happen in parallel, but writes
//! (index building) require exclusive access to avoid Lance's internal task conflicts.

use lancedb::{connect, Connection, Table};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};

/// Timeout for acquiring the write lock (in seconds)
const DB_WRITE_LOCK_TIMEOUT_SECS: u64 = 10;

/// Global state holding the RwLock and shared connections
struct DbState {
    /// RwLock for database operations - reads can be parallel, writes are exclusive
    rw_lock: RwLock<()>,
    /// Shared connections per database path
    connections: Mutex<HashMap<PathBuf, Connection>>,
}

static DB_STATE: OnceLock<DbState> = OnceLock::new();

fn get_db_state() -> &'static DbState {
    DB_STATE.get_or_init(|| DbState {
        rw_lock: RwLock::new(()),
        connections: Mutex::new(HashMap::new()),
    })
}

/// RAII guard that holds the database write lock (for index building operations)
pub struct DbWriteLockGuard<'a> {
    _guard: RwLockWriteGuard<'a, ()>,
}

/// Acquire an exclusive write lock on the database with a timeout.
/// Use this for operations that modify the database (create/drop tables, build indexes).
/// Returns a guard that releases the lock when dropped.
pub async fn acquire_write_lock() -> Result<DbWriteLockGuard<'static>, String> {
    tracing::debug!("db_lock: Attempting to acquire WRITE lock (timeout: {}s)", DB_WRITE_LOCK_TIMEOUT_SECS);
    let start = std::time::Instant::now();
    
    let state = get_db_state();
    
    match tokio::time::timeout(
        Duration::from_secs(DB_WRITE_LOCK_TIMEOUT_SECS),
        state.rw_lock.write(),
    )
    .await
    {
        Ok(guard) => {
            let elapsed = start.elapsed();
            tracing::debug!("db_lock: WRITE lock acquired in {:?}", elapsed);
            Ok(DbWriteLockGuard { _guard: guard })
        }
        Err(_) => {
            tracing::warn!("db_lock: Failed to acquire WRITE lock after {}s timeout", DB_WRITE_LOCK_TIMEOUT_SECS);
            Err(format!(
                "Database is busy with write operations, failed to acquire lock after {}s. Please retry.",
                DB_WRITE_LOCK_TIMEOUT_SECS
            ))
        }
    }
}

/// Get or create a shared LanceDB connection for a given database path.
/// This reuses connections to avoid Lance's internal task conflicts.
async fn get_or_create_connection(db_path: &Path) -> Result<Connection, String> {
    let state = get_db_state();
    let canonical_path = db_path.to_path_buf();
    
    // Check if we already have a connection
    {
        let connections = state.connections.lock().await;
        if let Some(conn) = connections.get(&canonical_path) {
            tracing::debug!("get_or_create_connection: Reusing existing connection for {:?}", db_path);
            return Ok(conn.clone());
        }
    }
    
    // Create new connection
    tracing::debug!("get_or_create_connection: Creating new connection for {:?}", db_path);
    fs::create_dir_all(db_path)
        .map_err(|e| format!("Failed to create db directory: {}", e))?;

    let db_uri = db_path.to_string_lossy().to_string();
    let conn = connect(&db_uri)
        .execute()
        .await
        .map_err(|e| format!("Failed to connect to LanceDB: {}", e))?;
    
    // Store the connection
    {
        let mut connections = state.connections.lock().await;
        connections.insert(canonical_path, conn.clone());
    }
    
    tracing::debug!("get_or_create_connection: New connection created and cached");
    Ok(conn)
}

/// Get a LanceDB connection for a given database path.
/// Uses a shared connection pool. No lock is acquired - use for read operations.
pub async fn get_connection(db_path: &Path) -> Result<Connection, String> {
    get_or_create_connection(db_path).await
}

/// Get a LanceDB connection with an exclusive write lock held.
/// Use this for operations that modify the database (create tables, build indexes).
/// Returns both the connection and the lock guard - the lock is released when the guard is dropped.
pub async fn get_connection_for_write(db_path: &Path) -> Result<(Connection, DbWriteLockGuard<'static>), String> {
    let lock_guard = acquire_write_lock().await?;
    let conn = get_connection(db_path).await?;
    Ok((conn, lock_guard))
}

/// Check if a table exists in the database.
/// This is a read operation - no exclusive lock is acquired.
pub async fn table_exists(db_path: &Path, table_name: &str) -> Result<bool, String> {
    tracing::debug!("common::table_exists: Checking for table '{}' at {:?}", table_name, db_path);
    
    let conn = get_connection(db_path).await?;
    tracing::debug!("common::table_exists: Got connection, listing tables");
    let table_names = conn
        .table_names()
        .execute()
        .await
        .map_err(|e| format!("Failed to list tables: {}", e))?;
    
    let exists = table_names.contains(&table_name.to_string());
    tracing::debug!("common::table_exists: Found {} tables, '{}' exists={}", table_names.len(), table_name, exists);

    Ok(exists)
}

/// Open a table in the database for reading.
/// This is a read operation - no exclusive lock is acquired.
#[allow(dead_code)]
pub async fn get_table(db_path: &Path, table_name: &str) -> Result<Table, String> {
    tracing::debug!("common::get_table: Opening table '{}' for reading", table_name);
    
    let conn = get_connection(db_path).await?;

    let table = conn.open_table(table_name)
        .execute()
        .await
        .map_err(|e| format!("Failed to open table: {}", e))?;
    
    tracing::debug!("common::get_table: Table '{}' opened successfully", table_name);
    Ok(table)
}
