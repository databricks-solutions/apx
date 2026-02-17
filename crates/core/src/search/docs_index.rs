//! SDK documentation indexing and search using SQLite FTS5.

use rayon::prelude::*;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::common::Timer;
use crate::databricks_sdk_doc::{SDKSource, download_and_extract_sdk, load_doc_files};
use crate::interop::get_databricks_sdk_version;
use crate::search::common;

const CHUNK_SIZE: usize = 2000; // characters (no tokenizer needed for FTS)
const CHUNK_OVERLAP: usize = 200; // characters overlap
const SCHEMA_VERSION: u32 = 1; // v1: Pure FTS (no embeddings)

/// Documentation chunk record for storage
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DocChunk {
    /// Unique ID for the chunk (file_path:chunk_index)
    pub id: String,
    /// The text content of the chunk
    pub text: String,
    /// Source file path relative to docs/
    pub source_file: String,
    /// Chunk index within the file
    pub chunk_index: usize,
    /// Service name (e.g., "clusters", "jobs", "warehouses")
    pub service: String,
    /// Entity/class name (e.g., "ClustersAPI", "ClustersExt")
    pub entity: String,
    /// Operation/method name (e.g., "create", "list", "delete")
    pub operation: String,
    /// Searchable symbols (concatenation of important identifiers)
    pub symbols: String,
}

/// Search result with score
#[derive(Debug, Clone, Serialize)]
pub struct DocSearchResult {
    pub text: String,
    pub source_file: String,
    pub score: f32,
}

/// Chunk text into overlapping segments with context headers
fn chunk_text(
    text: &str,
    file_path: &str,
    service: &str,
    entity: &str,
    operation: &str,
    symbols: &str,
) -> Vec<(String, String, usize, String, String, String, String)> {
    // Build context header
    let mut header_parts = Vec::new();
    if !entity.is_empty() {
        header_parts.push(entity.to_string());
    }
    if !service.is_empty() {
        header_parts.push(service.to_string());
    }
    if !operation.is_empty() {
        header_parts.push(operation.to_string());
    }

    let context_header = if header_parts.is_empty() {
        String::new()
    } else {
        format!("{} ", header_parts.join(" "))
    };

    // Prepend context header to text for chunking
    let enriched_text = if context_header.is_empty() {
        text.to_string()
    } else {
        format!("{context_header}{text}")
    };

    if enriched_text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut chunk_index = 0;
    let mut start = 0;
    let text_len = enriched_text.len();

    while start < text_len {
        let end = (start + CHUNK_SIZE).min(text_len);

        // Try to break at a word boundary
        let actual_end = if end < text_len {
            // Find last space before end
            enriched_text[start..end]
                .rfind(' ')
                .map(|pos| start + pos)
                .unwrap_or(end)
        } else {
            end
        };

        let chunk_text = &enriched_text[start..actual_end];

        if !chunk_text.trim().is_empty() {
            let chunk_id = format!("{file_path}:{chunk_index}");
            chunks.push((
                chunk_id,
                chunk_text.to_string(),
                chunk_index,
                service.to_string(),
                entity.to_string(),
                operation.to_string(),
                symbols.to_string(),
            ));
            chunk_index += 1;
        }

        // Move start forward with overlap
        if actual_end >= text_len {
            break;
        }
        start = actual_end.saturating_sub(CHUNK_OVERLAP);
        if start <= (actual_end - CHUNK_SIZE.min(actual_end)) {
            start = actual_end;
        }
    }

    chunks
}

/// SDK documentation index using SQLite FTS5
#[derive(Debug)]
pub struct SDKDocsIndex {
    conn: Arc<Mutex<Connection>>,
    version: Option<String>,
}

impl SDKDocsIndex {
    /// Create a new SDK docs index using the global search database
    pub fn new() -> Result<Self, String> {
        let conn = common::get_connection()?;
        Ok(Self {
            conn,
            version: None,
        })
    }

    /// Create with custom connection (for testing)
    #[allow(dead_code)]
    pub fn with_connection(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            version: None,
        }
    }

    /// Get table name for a version
    pub fn table_name(version: &str) -> String {
        format!(
            "sdk_docs_python_{}_v{}",
            version.replace('.', "_"),
            SCHEMA_VERSION
        )
    }

    /// Check if the index table exists
    fn table_exists_sync(&self, table_name: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;
        common::table_exists(&conn, table_name)
    }

    /// Bootstrap: download docs and build index
    #[allow(dead_code)]
    pub async fn bootstrap(&mut self, source: &SDKSource) -> Result<bool, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| "databricks-sdk is not installed".to_string())?;
                self.bootstrap_with_version(source, &version).await
            }
        }
    }

    /// Bootstrap with a pre-computed SDK version
    pub async fn bootstrap_with_version(
        &mut self,
        source: &SDKSource,
        version: &str,
    ) -> Result<bool, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                tracing::info!("Using Databricks SDK version: {}", version);
                self.version = Some(version.to_string());

                let table_name = Self::table_name(version);

                // Check if already indexed (sync, but cheap)
                if self.table_exists_sync(&table_name)? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }

                // Download and extract (async)
                let docs_path = download_and_extract_sdk(version).await?;

                // Build index (sync, wrapped in spawn_blocking by caller)
                self.build_index(&table_name, &docs_path)?;

                Ok(true)
            }
        }
    }

    /// Build index from a docs path (sync)
    fn build_index(&self, table_name: &str, docs_path: &std::path::Path) -> Result<(), String> {
        let overall_timer = Timer::start("build_index");

        // Load documentation files
        let load_timer = Timer::start("load_doc_files");
        tracing::info!(
            "Loading documentation files from docs/workspace/, docs/dbdataclasses/, and docs/*.md"
        );
        let files = load_doc_files(docs_path)?;
        load_timer.lap(&format!("Loaded {} documentation files", files.len()));

        // Chunk all files in parallel
        let chunk_timer = Timer::start("chunk_text_parallel");

        if let Some(doc) = files.first() {
            tracing::info!(
                "Sample metadata: file='{}', service='{}', entity='{}', operation='{}', symbols='{}'",
                doc.relative_path,
                doc.service,
                doc.entity,
                doc.operation,
                doc.symbols
            );
        }

        let doc_chunks: Vec<DocChunk> = files
            .par_iter()
            .flat_map(|doc| {
                chunk_text(
                    &doc.text,
                    &doc.relative_path,
                    &doc.service,
                    &doc.entity,
                    &doc.operation,
                    &doc.symbols,
                )
                .into_iter()
                .map(
                    |(id, chunk_text, chunk_index, svc, ent, op, syms)| DocChunk {
                        id,
                        text: chunk_text,
                        source_file: doc.relative_path.clone(),
                        chunk_index,
                        service: svc,
                        entity: ent,
                        operation: op,
                        symbols: syms,
                    },
                )
                .collect::<Vec<_>>()
            })
            .collect();

        chunk_timer.lap(&format!("Created {} text chunks", doc_chunks.len()));

        // Database operations
        let db_timer = Timer::start("database_operations");

        let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;

        // Drop existing table if it exists
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {table_name}"))
            .map_err(|e| format!("Failed to drop existing table: {e}"))?;

        // Create FTS5 virtual table
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE \"{table_name}\" USING fts5(\
                id UNINDEXED, text, source_file UNINDEXED, \
                chunk_index UNINDEXED, service, entity, operation, symbols, \
                tokenize='porter unicode61'\
            )"
        ))
        .map_err(|e| format!("Failed to create FTS5 table: {e}"))?;

        db_timer.lap("Created FTS5 table");

        // Insert all chunks in a transaction
        let insert_timer = Timer::start("insert_chunks");

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction error: {e}"))?;

        {
            let mut stmt = tx
                .prepare(&format!(
                    "INSERT INTO \"{table_name}\" \
                     (id, text, source_file, chunk_index, service, entity, operation, symbols) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ))
                .map_err(|e| format!("Prepare error: {e}"))?;

            for chunk in &doc_chunks {
                stmt.execute(rusqlite::params![
                    chunk.id,
                    chunk.text,
                    chunk.source_file,
                    chunk.chunk_index as i64,
                    chunk.service,
                    chunk.entity,
                    chunk.operation,
                    chunk.symbols,
                ])
                .map_err(|e| format!("Insert error: {e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {e}"))?;
        insert_timer.finish();

        db_timer.finish();
        overall_timer.finish();

        tracing::info!(
            "SDK docs FTS5 index built: {} chunks in table '{}'",
            doc_chunks.len(),
            table_name
        );
        Ok(())
    }

    /// Search for relevant documentation chunks using FTS5 (sync)
    pub fn search_sync(
        &self,
        source: &SDKSource,
        query: &str,
        limit: usize,
    ) -> Result<Vec<DocSearchResult>, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| {
                        "databricks-sdk is not installed. Please install databricks-sdk to use this feature.".to_string()
                    })?;

                let table_name = Self::table_name(&version);

                let conn = self.conn.lock().map_err(|e| format!("Lock error: {e}"))?;

                if !common::table_exists(&conn, &table_name)? {
                    return Err(format!(
                        "SDK docs not indexed for version {version}. Index will be built on next server start."
                    ));
                }

                let sanitized = common::sanitize_fts5_query(query);
                if sanitized.is_empty() {
                    return Ok(Vec::new());
                }

                tracing::debug!("search: Executing FTS5 query for '{}'", query);

                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT text, source_file, rank FROM \"{table_name}\" \
                         WHERE \"{table_name}\" MATCH ?1 \
                         ORDER BY rank \
                         LIMIT ?2"
                    ))
                    .map_err(|e| format!("Prepare error: {e}"))?;

                let rows = stmt
                    .query_map(rusqlite::params![sanitized, limit as i64], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, f64>(2)?,
                        ))
                    })
                    .map_err(|e| format!("Query error: {e}"))?;

                let mut results = Vec::new();

                for (rank, row_result) in rows.enumerate() {
                    let (text, source_file, _fts_rank) =
                        row_result.map_err(|e| format!("Row error: {e}"))?;

                    let score = 1.0 / (1.0 + rank as f32);
                    results.push(DocSearchResult {
                        text,
                        source_file,
                        score,
                    });
                }

                tracing::info!("FTS5 search for '{}': {} results", query, results.len());
                Ok(results)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_text_basic() {
        let text = "This is a test document about clusters API. The create method allows you to create new clusters.";
        let chunks = chunk_text(
            text,
            "test.rst",
            "clusters",
            "ClustersAPI",
            "create",
            "clusters create ClustersAPI",
        );

        assert!(!chunks.is_empty(), "Should create at least one chunk");

        let (id, chunk_text, idx, svc, ent, op, _syms) = &chunks[0];
        assert!(id.starts_with("test.rst:"));
        assert!(!chunk_text.is_empty());
        assert_eq!(*idx, 0);
        assert_eq!(svc, "clusters");
        assert_eq!(ent, "ClustersAPI");
        assert_eq!(op, "create");
    }

    #[test]
    fn test_chunk_text_long_document() {
        // Create a long document that should be split
        let text = "word ".repeat(1000);
        let chunks = chunk_text(&text, "test.rst", "", "", "", "");

        // Should create multiple chunks for long text
        assert!(
            chunks.len() > 1,
            "Long text should be split into multiple chunks"
        );

        // Verify chunk IDs are sequential
        for (i, (id, _, idx, _, _, _, _)) in chunks.iter().enumerate() {
            assert_eq!(*idx, i);
            assert!(id.contains(&format!(":{i}")));
        }
    }

    #[test]
    fn test_chunk_text_empty() {
        let chunks = chunk_text("", "test.rst", "", "", "", "");
        assert!(chunks.is_empty(), "Empty text should produce no chunks");
    }

    #[test]
    fn test_sdk_docs_index_creation() {
        let conn = Connection::open_in_memory().unwrap();
        let conn = Arc::new(Mutex::new(conn));
        let index = SDKDocsIndex::with_connection(conn);
        assert!(index.version.is_none());
    }

    #[test]
    fn test_fts5_search_with_data() {
        let conn = Connection::open_in_memory().unwrap();
        let table_name = "sdk_docs_fts_test_v1";

        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE \"{table_name}\" USING fts5(\
                id UNINDEXED, text, source_file UNINDEXED, \
                chunk_index UNINDEXED, service, entity, operation, symbols, \
                tokenize='porter unicode61'\
            )"
        ))
        .unwrap();

        conn.execute(
            &format!(
                "INSERT INTO \"{table_name}\" \
                 (id, text, source_file, chunk_index, service, entity, operation, symbols) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            rusqlite::params![
                "test.rst:0",
                "ClustersAPI clusters create This is about creating clusters",
                "test.rst",
                0,
                "clusters",
                "ClustersAPI",
                "create",
                "clusters create ClustersAPI"
            ],
        )
        .unwrap();

        // Verify the data is searchable
        let mut stmt = conn
            .prepare(&format!(
                "SELECT text, source_file FROM \"{table_name}\" \
                 WHERE \"{table_name}\" MATCH '\"clusters\"' LIMIT 5"
            ))
            .unwrap();

        let results: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(results.len(), 1);
        assert!(results[0].0.contains("clusters"));
    }
}
