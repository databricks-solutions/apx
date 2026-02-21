//! SDK documentation indexing and search using SQLite FTS5.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::common::Timer;
use crate::databricks_sdk_doc::{SDKSource, download_and_extract_sdk, load_doc_files};
use apx_db::fts::{Fts5Column, Fts5Table, enhance_fts5_query, sanitize_fts5_terms};

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

/// FTS5 column layout for SDK docs tables.
fn docs_fts_columns() -> Vec<Fts5Column> {
    vec![
        Fts5Column {
            name: "id",
            indexed: false,
        },
        Fts5Column {
            name: "text",
            indexed: true,
        },
        Fts5Column {
            name: "source_file",
            indexed: false,
        },
        Fts5Column {
            name: "chunk_index",
            indexed: false,
        },
        Fts5Column {
            name: "service",
            indexed: true,
        },
        Fts5Column {
            name: "entity",
            indexed: true,
        },
        Fts5Column {
            name: "operation",
            indexed: true,
        },
        Fts5Column {
            name: "symbols",
            indexed: true,
        },
    ]
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

    /// Build an [`Fts5Table`] handle for the given table name.
    fn fts_table(&self, table_name: &str) -> Result<Fts5Table, String> {
        Fts5Table::new(self.pool.clone(), table_name, docs_fts_columns())
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
                let fts = self.fts_table(&table_name)?;

                // Check if already indexed
                if fts.exists().await? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }

                // Download and extract (async)
                let docs_path = download_and_extract_sdk(version).await?;

                // Build index (async)
                self.build_index(&fts, &docs_path).await?;

                Ok(true)
            }
        }
    }

    /// Build index from a docs path
    async fn build_index(
        &self,
        fts: &Fts5Table,
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

        fts.create_or_replace().await?;

        db_timer.lap("Created FTS5 table");

        // Insert all chunks in a transaction
        let insert_timer = Timer::start("insert_chunks");

        let mut tx = fts.begin().await?;

        for chunk in &doc_chunks {
            let chunk_idx = chunk.chunk_index.to_string();
            fts.insert_str(
                &mut tx,
                &[
                    &chunk.id,
                    &chunk.text,
                    &chunk.source_file,
                    &chunk_idx,
                    &chunk.service,
                    &chunk.entity,
                    &chunk.operation,
                    &chunk.symbols,
                ],
            )
            .await?;
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
            fts.table_name()
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
                let fts = self.fts_table(&table_name)?;

                if !fts.exists().await? {
                    return Err(format!(
                        "SDK docs not indexed for version {version}. Index will be built on next server start."
                    ));
                }

                let terms = sanitize_fts5_terms(query);
                if terms.is_empty() {
                    return Ok(Vec::new());
                }

                // Enhance query: if it contains a PascalCase term, boost entity column
                let sanitized = enhance_fts5_query(&terms);

                tracing::debug!(
                    "search: Executing FTS5 query '{}' (original: '{}')",
                    sanitized,
                    query
                );

                // bm25 indexed-column weights (column order):
                //   text(1.0), service(1.0), entity(5.0), operation(1.0), symbols(3.0)
                let rows = fts
                    .search_bm25(
                        &sanitized,
                        &[1.0, 1.0, 5.0, 1.0, 3.0],
                        limit,
                        &["text", "source_file"],
                    )
                    .await?;

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

        let fts = Fts5Table::new(pool, table_name, docs_fts_columns()).unwrap();
        fts.create_or_replace().await.unwrap();

        let mut tx = fts.begin().await.unwrap();
        fts.insert_str(
            &mut tx,
            &[
                "test.rst:0",
                "ClustersAPI clusters create This is about creating clusters",
                "test.rst",
                "0",
                "clusters",
                "ClustersAPI",
                "create",
                "clusters create ClustersAPI",
            ],
        )
        .await
        .unwrap();
        tx.commit().await.map_err(|e| format!("{e}")).unwrap();

        // Verify the data is searchable
        let rows = fts
            .search("\"clusters\"", 5, &["text", "source_file"])
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        let text: String = rows[0].get("text");
        assert!(text.contains("clusters"));
    }

    /// End-to-end: insert a doc row, search with a multi-word query, verify no FTS5 error.
    #[tokio::test]
    async fn test_fts5_search_multiword_no_error() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let table_name = "sdk_docs_fts_multiword_v1";

        let fts = Fts5Table::new(pool, table_name, docs_fts_columns()).unwrap();
        fts.create_or_replace().await.unwrap();

        let mut tx = fts.begin().await.unwrap();
        fts.insert_str(
            &mut tx,
            &[
                "serving.rst:0",
                "Guide to serving endpoints and model serving",
                "serving.rst",
                "0",
                "serving",
                "",
                "",
                "serving endpoints",
            ],
        )
        .await
        .unwrap();
        tx.commit().await.map_err(|e| format!("{e}")).unwrap();

        // Build the query exactly as the search method does
        let terms = sanitize_fts5_terms("serving endpoints");
        let query = enhance_fts5_query(&terms);

        let rows = fts
            .search(&query, 5, &["text"])
            .await
            .expect("multi-word FTS5 query must not fail");

        assert_eq!(rows.len(), 1);
    }
}
