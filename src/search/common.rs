//! Common utilities for working with LanceDB indices

use lancedb::{connect, Connection, Table};
use std::fs;
use std::path::Path;

/// Get a LanceDB connection for a given database path
pub async fn get_connection(db_path: &Path) -> Result<Connection, String> {
    tracing::debug!("get_connection: Creating dir and connecting to {:?}", db_path);
    fs::create_dir_all(db_path)
        .map_err(|e| format!("Failed to create db directory: {}", e))?;

    let db_uri = db_path.to_string_lossy().to_string();
    tracing::debug!("get_connection: Connecting to db_uri={}", db_uri);
    let conn = connect(&db_uri)
        .execute()
        .await
        .map_err(|e| format!("Failed to connect to LanceDB: {}", e))?;
    tracing::debug!("get_connection: Connected successfully");
    Ok(conn)
}

/// Check if a table exists in the database
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

/// Open a table in the database
pub async fn get_table(db_path: &Path, table_name: &str) -> Result<Table, String> {
    let conn = get_connection(db_path).await?;

    conn.open_table(table_name)
        .execute()
        .await
        .map_err(|e| format!("Failed to open table: {}", e))
}
