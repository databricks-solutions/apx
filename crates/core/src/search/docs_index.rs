//! SDK documentation indexing and search using SQLite FTS5.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::common::Timer;
use crate::databricks_sdk_doc::{SDKSource, download_and_extract_sdk, load_doc_files};
use apx_db::dev::{sanitize_fts5_query, table_exists};

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
#[derive(Debug, Clone)]
pub struct SDKDocsIndex {
    pool: SqlitePool,
    version: Option<String>,
}

impl SDKDocsIndex {
    /// Create a new SDK docs index using the provided pool
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            version: None,
        }
    }

    /// Create with a specific pool (for testing or custom setups)
    #[allow(dead_code)]
    pub fn with_pool(pool: SqlitePool) -> Self {
        Self {
            pool,
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
    async fn table_exists_check(&self, table_name: &str) -> Result<bool, String> {
        table_exists(&self.pool, table_name).await
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

                // Check if already indexed
                if self.table_exists_check(&table_name).await? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }

                // Download and extract (async)
                let docs_path = download_and_extract_sdk(version).await?;

                // Build index (async)
                self.build_index(&table_name, &docs_path).await?;

                Ok(true)
            }
        }
    }

    /// Build index from a docs path
    async fn build_index(
        &self,
        table_name: &str,
        docs_path: &std::path::Path,
    ) -> Result<(), String> {
        let overall_timer = Timer::start("build_index");

        // Load documentation files
        let load_timer = Timer::start("load_doc_files");
        tracing::info!(
            "Loading documentation files from docs/workspace/, docs/dbdataclasses/, and docs/*.md"
        );
        let files = load_doc_files(docs_path)?;
        load_timer.lap(&format!("Loaded {} documentation files", files.len()));

        // Chunk all files in parallel (CPU-bound, uses rayon)
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

        // Database operations (async)
        let db_timer = Timer::start("database_operations");

        // Drop existing table if it exists
        sqlx::query(&format!("DROP TABLE IF EXISTS \"{table_name}\""))
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to drop existing table: {e}"))?;

        // Create FTS5 virtual table
        sqlx::query(&format!(
            "CREATE VIRTUAL TABLE \"{table_name}\" USING fts5(\
                id UNINDEXED, text, source_file UNINDEXED, \
                chunk_index UNINDEXED, service, entity, operation, symbols, \
                tokenize='porter unicode61'\
            )"
        ))
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create FTS5 table: {e}"))?;

        db_timer.lap("Created FTS5 table");

        // Insert all chunks in a transaction
        let insert_timer = Timer::start("insert_chunks");

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Transaction error: {e}"))?;

        for chunk in &doc_chunks {
            sqlx::query(&format!(
                "INSERT INTO \"{table_name}\" \
                 (id, text, source_file, chunk_index, service, entity, operation, symbols) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ))
            .bind(&chunk.id)
            .bind(&chunk.text)
            .bind(&chunk.source_file)
            .bind(chunk.chunk_index as i64)
            .bind(&chunk.service)
            .bind(&chunk.entity)
            .bind(&chunk.operation)
            .bind(&chunk.symbols)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Insert error: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Commit error: {e}"))?;
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

    /// Switch to a different SDK version, bootstrapping it if needed.
    ///
    /// This is cheap when the version is already indexed (just a `table_exists` check),
    /// and lazy-downloads otherwise.
    pub async fn ensure_version(
        &mut self,
        source: &SDKSource,
        version: &str,
    ) -> Result<(), String> {
        if self.version.as_deref() == Some(version) {
            return Ok(()); // Already on this version
        }
        self.bootstrap_with_version(source, version).await?;
        Ok(())
    }

    /// Search for relevant documentation chunks using FTS5
    pub async fn search(
        &self,
        source: &SDKSource,
        query: &str,
        limit: usize,
    ) -> Result<Vec<DocSearchResult>, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                let version = self.version.as_ref().ok_or_else(|| {
                    "SDK docs index not initialized. No version has been bootstrapped.".to_string()
                })?;

                let table_name = Self::table_name(version);

                if !table_exists(&self.pool, &table_name).await? {
                    return Err(format!(
                        "SDK docs not indexed for version {version}. Index will be built on next server start."
                    ));
                }

                let sanitized = sanitize_fts5_query(query);
                if sanitized.is_empty() {
                    return Ok(Vec::new());
                }

                tracing::debug!("search: Executing FTS5 query for '{}'", query);

                let rows = sqlx::query(&format!(
                    "SELECT text, source_file, rank FROM \"{table_name}\" \
                     WHERE \"{table_name}\" MATCH ?1 \
                     ORDER BY rank \
                     LIMIT ?2"
                ))
                .bind(&sanitized)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Query error: {e}"))?;

                let mut results = Vec::new();

                for (rank, row) in rows.iter().enumerate() {
                    let text: String = row.get("text");
                    let source_file: String = row.get("source_file");

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

    #[tokio::test]
    async fn test_sdk_docs_index_creation() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let index = SDKDocsIndex::with_pool(pool);
        assert!(index.version.is_none());
    }

    #[tokio::test]
    async fn test_fts5_search_with_data() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let table_name = "sdk_docs_fts_test_v1";

        sqlx::query(&format!(
            "CREATE VIRTUAL TABLE \"{table_name}\" USING fts5(\
                id UNINDEXED, text, source_file UNINDEXED, \
                chunk_index UNINDEXED, service, entity, operation, symbols, \
                tokenize='porter unicode61'\
            )"
        ))
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(&format!(
            "INSERT INTO \"{table_name}\" \
             (id, text, source_file, chunk_index, service, entity, operation, symbols) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ))
        .bind("test.rst:0")
        .bind("ClustersAPI clusters create This is about creating clusters")
        .bind("test.rst")
        .bind(0i64)
        .bind("clusters")
        .bind("ClustersAPI")
        .bind("create")
        .bind("clusters create ClustersAPI")
        .execute(&pool)
        .await
        .unwrap();

        // Verify the data is searchable
        let results = sqlx::query(&format!(
            "SELECT text, source_file FROM \"{table_name}\" \
             WHERE \"{table_name}\" MATCH '\"clusters\"' LIMIT 5"
        ))
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        let text: String = results[0].get("text");
        assert!(text.contains("clusters"));
    }
}
