//! SDK documentation indexing and search using LanceDB with Full-Text Search.

use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::index::scalar::FullTextSearchQuery;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use arrow::array::{StringArray, ArrayRef};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};
use futures_util::StreamExt;

use crate::search::common;
use crate::common::Timer;
use crate::databricks_sdk_doc::{SDKSource, download_and_extract_sdk, load_doc_files};
use crate::interop::get_databricks_sdk_version;

const CHUNK_SIZE: usize = 2000; // characters (no tokenizer needed for FTS)
const CHUNK_OVERLAP: usize = 200; // characters overlap
const SCHEMA_VERSION: u32 = 1; // v1: Pure FTS (no embeddings)

/// Documentation chunk record for LanceDB storage
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
        format!("{}{}", context_header, text)
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
            let chunk_id = format!("{}:{}", file_path, chunk_index);
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

/// Helper function to convert DocChunk vector to Arrow RecordBatch
fn chunks_to_batch(chunks: Vec<DocChunk>) -> Result<RecordBatch, String> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("source_file", DataType::Utf8, false),
        Field::new("chunk_index", DataType::UInt64, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("entity", DataType::Utf8, false),
        Field::new("operation", DataType::Utf8, false),
        Field::new("symbols", DataType::Utf8, false),
    ]);

    // Create arrays
    let id_array = StringArray::from(
        chunks.iter().map(|c| c.id.as_str()).collect::<Vec<_>>()
    );

    let text_array = StringArray::from(
        chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );

    let source_file_array = StringArray::from(
        chunks.iter().map(|c| c.source_file.as_str()).collect::<Vec<_>>()
    );

    let chunk_index_array = arrow::array::UInt64Array::from(
        chunks.iter().map(|c| c.chunk_index as u64).collect::<Vec<_>>()
    );

    let service_array = StringArray::from(
        chunks.iter().map(|c| c.service.as_str()).collect::<Vec<_>>()
    );

    let entity_array = StringArray::from(
        chunks.iter().map(|c| c.entity.as_str()).collect::<Vec<_>>()
    );

    let operation_array = StringArray::from(
        chunks.iter().map(|c| c.operation.as_str()).collect::<Vec<_>>()
    );

    let symbols_array = StringArray::from(
        chunks.iter().map(|c| c.symbols.as_str()).collect::<Vec<_>>()
    );

    // Create record batch
    RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(id_array) as ArrayRef,
            std::sync::Arc::new(text_array) as ArrayRef,
            std::sync::Arc::new(source_file_array) as ArrayRef,
            std::sync::Arc::new(chunk_index_array) as ArrayRef,
            std::sync::Arc::new(service_array) as ArrayRef,
            std::sync::Arc::new(entity_array) as ArrayRef,
            std::sync::Arc::new(operation_array) as ArrayRef,
            std::sync::Arc::new(symbols_array) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to create record batch: {}", e))
}

/// SDK documentation index using LanceDB with Full-Text Search
pub struct SDKDocsIndex {
    db_path: PathBuf,
    version: Option<String>,
}

impl SDKDocsIndex {
    /// Create a new SDK docs index
    pub fn new() -> Result<Self, String> {
        let db_path = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?
            .join(".apx")
            .join("db");

        Ok(Self {
            db_path,
            version: None,
        })
    }

    /// Create with custom db path (for testing)
    #[allow(dead_code)]
    pub fn with_db_path(db_path: PathBuf) -> Result<Self, String> {
        Ok(Self {
            db_path,
            version: None,
        })
    }

    /// Get table name for a version
    pub fn table_name(version: &str) -> String {
        format!("sdk_docs_python_{}_schema_v{}", version.replace('.', "_"), SCHEMA_VERSION)
    }

    /// Check if the index table exists
    async fn table_exists(&self, table_name: &str) -> Result<bool, String> {
        tracing::debug!("table_exists: Checking for table '{}' in db at {:?}", table_name, self.db_path);
        let result = common::table_exists(&self.db_path, table_name).await;
        tracing::debug!("table_exists: Result for '{}': {:?}", table_name, result);
        result
    }

    /// Bootstrap: download docs and build index
    /// 
    /// This method gets the SDK version via Python interop. If calling from an async
    /// context where Python GIL might cause issues, use `bootstrap_with_version` instead.
    #[allow(dead_code)]
    pub async fn bootstrap(&mut self, source: &SDKSource) -> Result<bool, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                tracing::debug!("bootstrap: Starting SDK docs bootstrap for DatabricksSdkPython");
                
                // Get SDK version
                tracing::debug!("bootstrap: Getting Databricks SDK version via Python interop");
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| "databricks-sdk is not installed".to_string())?;

                self.bootstrap_with_version(source, &version).await
            }
        }
    }

    /// Bootstrap with a pre-computed SDK version
    /// 
    /// Use this method when the SDK version has been computed outside of an async context
    /// to avoid Python GIL issues with PyO3.
    pub async fn bootstrap_with_version(&mut self, source: &SDKSource, version: &str) -> Result<bool, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                tracing::debug!("bootstrap_with_version: Starting SDK docs bootstrap for DatabricksSdkPython");
                tracing::info!("Using Databricks SDK version: {}", version);
                self.version = Some(version.to_string());

                let table_name = Self::table_name(version);
                tracing::debug!("bootstrap_with_version: Table name will be: {}", table_name);

                // Check if already indexed
                tracing::debug!("bootstrap_with_version: Checking if table already exists");
                if self.table_exists(&table_name).await? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }
                tracing::debug!("bootstrap_with_version: Table does not exist, need to build index");

                // Download and extract
                tracing::debug!("bootstrap_with_version: Starting download_and_extract_sdk for version {}", version);
                let docs_path = download_and_extract_sdk(version).await?;
                tracing::debug!("bootstrap_with_version: SDK docs extracted to {:?}", docs_path);

                // Build index
                tracing::debug!("bootstrap_with_version: Starting build_index for table {}", table_name);
                self.build_index(&table_name, &docs_path).await?;
                tracing::debug!("bootstrap_with_version: build_index completed successfully");

                Ok(true)
            }
        }
    }

    /// Build index from a docs path
    async fn build_index(&self, table_name: &str, docs_path: &std::path::Path) -> Result<(), String> {
        let overall_timer = Timer::start("build_index");
        tracing::debug!("build_index: Starting index build for table '{}' from path {:?}", table_name, docs_path);

        // Load documentation files
        let load_timer = Timer::start("load_doc_files");
        tracing::info!("Loading documentation files from docs/workspace/, docs/dbdataclasses/, and docs/*.md");
        let files = load_doc_files(docs_path)?;
        load_timer.lap(&format!("Loaded {} documentation files", files.len()));

        // Chunk all files in parallel
        let chunk_timer = Timer::start("chunk_text_parallel");
        
        // Log first file to verify metadata extraction
        if let Some(doc) = files.first() {
            tracing::info!(
                "Sample metadata: file='{}', service='{}', entity='{}', operation='{}', symbols='{}'",
                doc.relative_path, doc.service, doc.entity, doc.operation, doc.symbols
            );
        }
        
        let all_chunks: Vec<(String, String, String, usize, String, String, String, String)> = files.par_iter()
            .flat_map(|doc| {
                chunk_text(
                    &doc.text, 
                    &doc.relative_path, 
                    &doc.service, 
                    &doc.entity, 
                    &doc.operation, 
                    &doc.symbols
                )
                .into_iter()
                .map(|(id, chunk_text, chunk_index, svc, ent, op, syms)| {
                    (id, chunk_text, doc.relative_path.clone(), chunk_index, svc, ent, op, syms)
                })
                .collect::<Vec<_>>()
            })
            .collect();
        
        chunk_timer.lap(&format!("Created {} text chunks", all_chunks.len()));

        // Create doc chunks (no embeddings needed)
        let doc_chunks: Vec<DocChunk> = all_chunks
            .into_iter()
            .map(|(id, text, source_file, chunk_index, service, entity, operation, symbols)| {
                DocChunk {
                    id,
                    text,
                    source_file,
                    chunk_index,
                    service,
                    entity,
                    operation,
                    symbols,
                }
            })
            .collect();

        // Acquire WRITE lock for all DB write operations
        let db_timer = Timer::start("database_operations");
        tracing::debug!("build_index: Acquiring WRITE lock for table creation");
        let (conn, _lock) = common::get_connection_for_write(&self.db_path).await?;
        db_timer.lap("Connected to LanceDB with WRITE lock");

        // Drop existing table if it exists (check without separate lock since we hold it)
        let table_names = conn
            .table_names()
            .execute()
            .await
            .map_err(|e| format!("Failed to list tables: {}", e))?;
        
        if table_names.contains(&table_name.to_string()) {
            tracing::debug!("Dropping existing table: {}", table_name);
            conn.drop_table(table_name, &[])
                .await
                .map_err(|e| format!("Failed to drop existing table: {}", e))?;
            db_timer.lap("Dropped existing table");
        }

        // Convert to Arrow format
        let arrow_timer = Timer::start("arrow_conversion");
        let batch = chunks_to_batch(doc_chunks)?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(
            vec![Ok(batch)].into_iter(),
            schema,
        );
        arrow_timer.finish();

        let create_timer = Timer::start("create_table");
        let table = conn.create_table(table_name, Box::new(batches))
            .execute()
            .await
            .map_err(|e| format!("Failed to create table: {}", e))?;
        create_timer.finish();

        // Create FTS index on text column
        let fts_timer = Timer::start("create_fts_index");
        table.create_index(
            &["text"], 
            lancedb::index::Index::FTS(lancedb::index::scalar::FtsIndexBuilder::default())
        )
            .execute()
            .await
            .map_err(|e| format!("Failed to create FTS index: {}", e))?;
        fts_timer.finish();

        db_timer.finish();
        overall_timer.finish();
        tracing::debug!("build_index: Releasing WRITE lock");

        Ok(())
    }

    /// Search for relevant documentation chunks using Full-Text Search
    /// This is a READ operation - no write lock needed, can run in parallel with other reads.
    pub async fn search(
        &self,
        source: &SDKSource,
        query: &str,
        limit: usize,
    ) -> Result<Vec<DocSearchResult>, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| "databricks-sdk is not installed. Please install databricks-sdk to use this feature.".to_string())?;

                let table_name = Self::table_name(&version);

                // Get connection for read (no write lock needed)
                tracing::debug!("search: Starting search for query '{}'", query);
                let conn = common::get_connection(&self.db_path).await?;

                // Check table exists
                let table_names = conn
                    .table_names()
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to list tables: {}", e))?;
                
                if !table_names.contains(&table_name.to_string()) {
                    return Err(format!(
                        "SDK docs not indexed for version {}. Index will be built on next server start.",
                        version
                    ));
                }

                // Open table
                let table = conn.open_table(&table_name)
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to open table: {}", e))?;

                // Execute FTS search
                tracing::debug!("search: Executing FTS query");
                let fts_query = FullTextSearchQuery::new(query.to_string());
                let mut fts_results = match table
                    .query()
                    .full_text_search(fts_query)
                    .limit(limit)
                    .execute()
                    .await
                {
                    Ok(results) => {
                        tracing::debug!("search: FTS query executed successfully, reading results");
                        results
                    }
                    Err(e) => {
                        tracing::error!("search: FTS query execution failed: {}", e);
                        return Err(format!("Failed to execute FTS search: {}", e));
                    }
                };

                let mut results = Vec::new();
                let mut rank = 0;

                tracing::debug!("search: Reading result batches");
                while let Some(batch_result) = fts_results.next().await {
                    let batch = match batch_result {
                        Ok(b) => {
                            tracing::debug!("search: Got batch with {} rows", b.num_rows());
                            b
                        }
                        Err(e) => {
                            tracing::error!("search: Failed to read FTS batch: {}", e);
                            return Err(format!("Failed to read FTS batch: {}", e));
                        }
                    };
                    
                    let text_array = batch
                        .column_by_name("text")
                        .ok_or("Missing text column")?
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or("Failed to downcast text column")?;

                    let source_file_array = batch
                        .column_by_name("source_file")
                        .ok_or("Missing source_file column")?
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or("Failed to downcast source_file column")?;

                    for i in 0..batch.num_rows() {
                        // Simple rank-based scoring (higher rank = lower score)
                        let score = 1.0 / (1.0 + rank as f32);
                        
                        results.push(DocSearchResult {
                            text: text_array.value(i).to_string(),
                            source_file: source_file_array.value(i).to_string(),
                            score,
                        });
                        rank += 1;
                    }
                }

                tracing::info!(
                    "FTS search for '{}': {} results", 
                    query, results.len()
                );

                Ok(results)
            }
        }
    }
}

#[cfg(test)]
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
            "clusters create ClustersAPI"
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
        assert!(chunks.len() > 1, "Long text should be split into multiple chunks");
        
        // Verify chunk IDs are sequential
        for (i, (id, _, idx, _, _, _, _)) in chunks.iter().enumerate() {
            assert_eq!(*idx, i);
            assert!(id.contains(&format!(":{}", i)));
        }
    }

    #[test]
    fn test_chunk_text_empty() {
        let chunks = chunk_text("", "test.rst", "", "", "", "");
        assert!(chunks.is_empty(), "Empty text should produce no chunks");
    }

    #[tokio::test]
    async fn test_sdk_docs_index_creation() {
        use tempfile::TempDir;
        
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test_db");
        
        let index = SDKDocsIndex::with_db_path(db_path).expect("Failed to create index");
        assert!(index.version.is_none());
    }
}
