/// Common utilities for working with LanceDB indices

use lancedb::{connect, Connection, Table};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fs;
use std::path::Path;

use super::embedded_model::EMBEDDING_DIM;

/// Serde serialization module for fixed-size embedding arrays
pub mod serde_arrays {
    use super::*;

    pub fn serialize<S>(arr: &[f32; EMBEDDING_DIM], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        arr.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[f32; EMBEDDING_DIM], D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec = Vec::<f32>::deserialize(deserializer)?;
        let arr: [f32; EMBEDDING_DIM] = vec.try_into()
            .map_err(|v: Vec<f32>| {
                serde::de::Error::custom(format!(
                    "Expected array of length {}, got {}",
                    EMBEDDING_DIM,
                    v.len()
                ))
            })?;
        Ok(arr)
    }
}

/// Get a LanceDB connection for a given database path
pub async fn get_connection(db_path: &Path) -> Result<Connection, String> {
    fs::create_dir_all(db_path)
        .map_err(|e| format!("Failed to create db directory: {}", e))?;

    let db_uri = db_path.to_string_lossy().to_string();
    connect(&db_uri)
        .execute()
        .await
        .map_err(|e| format!("Failed to connect to LanceDB: {}", e))
}

/// Check if a table exists in the database
pub async fn table_exists(db_path: &Path, table_name: &str) -> Result<bool, String> {
    let conn = get_connection(db_path).await?;
    let table_names = conn
        .table_names()
        .execute()
        .await
        .map_err(|e| format!("Failed to list tables: {}", e))?;

    Ok(table_names.contains(&table_name.to_string()))
}

/// Open a table in the database
pub async fn get_table(db_path: &Path, table_name: &str) -> Result<Table, String> {
    let conn = get_connection(db_path).await?;

    conn.open_table(table_name)
        .execute()
        .await
        .map_err(|e| format!("Failed to open table: {}", e))
}
