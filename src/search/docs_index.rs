//! SDK documentation indexing and search using LanceDB with hybrid vector + FTS search.

use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::index::scalar::FullTextSearchQuery;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use arrow::array::{Float32Array, StringArray, FixedSizeListArray, ArrayRef};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};
use futures_util::StreamExt;
use tokenizers::Tokenizer;

use crate::search::embedder::Embedder;
use crate::search::embedded_model::{EMBEDDING_DIM, TOKENIZER_JSON};
use crate::search::common;
use crate::common::Timer;
use crate::databricks_sdk_doc::{SDKSource, download_and_extract_sdk, load_doc_files};
use crate::interop::get_databricks_sdk_version;

const CHUNK_SIZE: usize = 512; // tokens
const CHUNK_OVERLAP: usize = 128; // tokens - 25% overlap
const SCHEMA_VERSION: u32 = 7; // v7: ONNX embeddings with bge-small-en-v1.5

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
    /// Embedding vector
    #[serde(with = "common::serde_arrays")]
    pub embedding: [f32; EMBEDDING_DIM],
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

/// Candidate from search results with metadata for hybrid ranking
struct SearchCandidate {
    id: String,
    text: String,
    source_file: String,
    service: String,
    operation: String,
    symbols: String,
    vector_rank: Option<usize>,
    fts_rank: Option<usize>,
}

/// Chunk text into overlapping segments with context headers
fn chunk_text(
    text: &str, 
    tokenizer: &Tokenizer, 
    file_path: &str,
    service: &str,
    entity: &str,
    operation: &str,
    symbols: &str,
) -> Result<Vec<(String, String, usize, String, String, String, String)>, String> {
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

    let encoding = tokenizer.encode(enriched_text.as_str(), false)
        .map_err(|e| format!("Tokenization failed: {}", e))?;

    let tokens = encoding.get_ids();
    let offsets = encoding.get_offsets();

    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    let mut chunk_index = 0;
    let mut start = 0;

    while start < tokens.len() {
        let end = (start + CHUNK_SIZE).min(tokens.len());

        // Get text span for this chunk
        let start_offset = offsets[start].0;
        let end_offset = if end < offsets.len() {
            offsets[end - 1].1
        } else {
            enriched_text.len()
        };

        let chunk_text = &enriched_text[start_offset..end_offset];

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
        if end >= tokens.len() {
            break;
        }
        start += CHUNK_SIZE - CHUNK_OVERLAP;
    }

    Ok(chunks)
}

/// Helper function to convert DocChunk vector to Arrow RecordBatch
fn chunks_to_batch(chunks: Vec<DocChunk>) -> Result<RecordBatch, String> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("source_file", DataType::Utf8, false),
        Field::new("chunk_index", DataType::UInt64, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new(
                    "item",
                    DataType::Float32,
                    true,
                )),
                EMBEDDING_DIM as i32,
            ),
            false,
        ),
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

    // Create embedding array
    let mut embedding_values: Vec<f32> = Vec::with_capacity(chunks.len() * EMBEDDING_DIM);
    for chunk in &chunks {
        embedding_values.extend_from_slice(&chunk.embedding);
    }

    let values = Float32Array::from(embedding_values);
    let field = Field::new("item", DataType::Float32, true);
    let embedding_array = FixedSizeListArray::try_new(
        std::sync::Arc::new(field),
        EMBEDDING_DIM as i32,
        std::sync::Arc::new(values),
        None,
    ).map_err(|e| format!("Failed to create fixed size list array: {}", e))?;

    // Create metadata arrays
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
            std::sync::Arc::new(embedding_array) as ArrayRef,
            std::sync::Arc::new(service_array) as ArrayRef,
            std::sync::Arc::new(entity_array) as ArrayRef,
            std::sync::Arc::new(operation_array) as ArrayRef,
            std::sync::Arc::new(symbols_array) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to create record batch: {}", e))
}

/// SDK documentation index using LanceDB with hybrid vector + FTS search
pub struct SDKDocsIndex {
    db_path: PathBuf,
    embedder: Arc<Embedder>,
    tokenizer: Arc<Tokenizer>,
    version: Option<String>,
}

impl SDKDocsIndex {
    /// Create a new SDK docs index
    pub fn new() -> Result<Self, String> {
        let db_path = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?
            .join(".apx")
            .join("db");

        let embedder = Embedder::new()?;

        // Load tokenizer from embedded model
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        Ok(Self {
            db_path,
            embedder: Arc::new(embedder),
            tokenizer: Arc::new(tokenizer),
            version: None,
        })
    }

    /// Create with custom db path (for testing)
    #[allow(dead_code)]
    pub fn with_db_path(db_path: PathBuf) -> Result<Self, String> {
        let embedder = Embedder::new()?;
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        Ok(Self {
            db_path,
            embedder: Arc::new(embedder),
            tokenizer: Arc::new(tokenizer),
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
                    &self.tokenizer, 
                    &doc.relative_path, 
                    &doc.service, 
                    &doc.entity, 
                    &doc.operation, 
                    &doc.symbols
                )
                .unwrap_or_default()
                .into_iter()
                .map(|(id, chunk_text, chunk_index, svc, ent, op, syms)| {
                    (id, chunk_text, doc.relative_path.clone(), chunk_index, svc, ent, op, syms)
                })
                .collect::<Vec<_>>()
            })
            .collect();
        
        chunk_timer.lap(&format!("Created {} text chunks", all_chunks.len()));

        // Generate embeddings with batching
        let embed_timer = Timer::start("embed_batch");
        tracing::info!("Generating embeddings for {} chunks", all_chunks.len());
        let texts: Vec<String> = all_chunks.iter().map(|(_, text, _, _, _, _, _, _)| text.clone()).collect();
        let embeddings = self.embedder.embed_batch(&texts)?;
        embed_timer.lap(&format!("Generated {} embeddings", embeddings.len()));

        // Create doc chunks
        let doc_chunks: Vec<DocChunk> = all_chunks
            .into_iter()
            .zip(embeddings.into_iter())
            .map(|((id, text, source_file, chunk_index, service, entity, operation, symbols), emb)| {
                let emb_array: [f32; EMBEDDING_DIM] = emb.try_into()
                    .expect("Embedding should have correct length");

                DocChunk {
                    id,
                    text,
                    source_file,
                    chunk_index,
                    embedding: emb_array,
                    service,
                    entity,
                    operation,
                    symbols,
                }
            })
            .collect();

        // Create table
        let db_timer = Timer::start("database_operations");
        tracing::debug!("build_index: Getting LanceDB connection at {:?}", self.db_path);
        let conn = common::get_connection(&self.db_path).await?;
        db_timer.lap("Connected to LanceDB");

        // Drop existing table if it exists
        if self.table_exists(table_name).await? {
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

        // Create FTS index on text column for hybrid search
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

        Ok(())
    }

    /// Extract candidates from a record batch stream
    async fn extract_candidates_from_batch(
        batch: &RecordBatch,
        rank_start: usize,
    ) -> Result<Vec<(String, String, String, String, String, String, usize)>, String> {
        let mut results = Vec::new();
        
        let id_array = batch
            .column_by_name("id")
            .ok_or("Missing id column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Failed to downcast id column")?;

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

        let service_array = batch
            .column_by_name("service")
            .ok_or("Missing service column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Failed to downcast service column")?;

        let operation_array = batch
            .column_by_name("operation")
            .ok_or("Missing operation column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Failed to downcast operation column")?;

        let symbols_array = batch
            .column_by_name("symbols")
            .ok_or("Missing symbols column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Failed to downcast symbols column")?;

        for i in 0..batch.num_rows() {
            results.push((
                id_array.value(i).to_string(),
                text_array.value(i).to_string(),
                source_file_array.value(i).to_string(),
                service_array.value(i).to_string(),
                operation_array.value(i).to_string(),
                symbols_array.value(i).to_string(),
                rank_start + i,
            ));
        }
        
        Ok(results)
    }

    /// Compute RRF (Reciprocal Rank Fusion) score
    fn compute_rrf_score(vector_rank: Option<usize>, fts_rank: Option<usize>, k: f32) -> f32 {
        let mut score = 0.0;
        if let Some(rank) = vector_rank {
            score += 1.0 / (k + rank as f32);
        }
        if let Some(rank) = fts_rank {
            score += 1.0 / (k + rank as f32);
        }
        score
    }

    /// Search for relevant documentation chunks using hybrid vector + FTS search
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

                if !self.table_exists(&table_name).await? {
                    return Err(format!(
                        "SDK docs not indexed for version {}. Index will be built on next server start.",
                        version
                    ));
                }

                let query_embedding = self.embedder.embed(query)?;
                let table = common::get_table(&self.db_path, &table_name).await?;
                
                // Fetch candidate pool size (more candidates for better fusion)
                let candidate_pool = (limit * 10).max(100);

                // === Vector Search ===
                let mut vector_results = table
                    .query()
                    .nearest_to(query_embedding)
                    .map_err(|e| format!("Failed to create vector query: {}", e))?
                    .limit(candidate_pool)
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to execute vector search: {}", e))?;

                let mut candidates: HashMap<String, SearchCandidate> = HashMap::new();
                let mut rank = 0;
                
                while let Some(batch_result) = vector_results.next().await {
                    let batch = batch_result.map_err(|e| format!("Failed to read vector batch: {}", e))?;
                    let extracted = Self::extract_candidates_from_batch(&batch, rank).await?;
                    
                    for (id, text, source_file, service, operation, symbols, r) in extracted {
                        candidates.insert(id.clone(), SearchCandidate {
                            id,
                            text,
                            source_file,
                            service,
                            operation,
                            symbols,
                            vector_rank: Some(r),
                            fts_rank: None,
                        });
                    }
                    rank += batch.num_rows();
                }
                
                tracing::debug!("Vector search returned {} candidates", candidates.len());

                // === FTS Search ===
                let fts_query = FullTextSearchQuery::new(query.to_string());
                let mut fts_results = table
                    .query()
                    .full_text_search(fts_query)
                    .limit(candidate_pool)
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to execute FTS search: {}", e))?;

                let mut fts_rank = 0;
                while let Some(batch_result) = fts_results.next().await {
                    let batch = batch_result.map_err(|e| format!("Failed to read FTS batch: {}", e))?;
                    let extracted = Self::extract_candidates_from_batch(&batch, fts_rank).await?;
                    
                    for (id, text, source_file, service, operation, symbols, r) in extracted {
                        if let Some(existing) = candidates.get_mut(&id) {
                            // Already in vector results, add FTS rank
                            existing.fts_rank = Some(r);
                        } else {
                            // New candidate from FTS only
                            candidates.insert(id.clone(), SearchCandidate {
                                id,
                                text,
                                source_file,
                                service,
                                operation,
                                symbols,
                                vector_rank: None,
                                fts_rank: Some(r),
                            });
                        }
                    }
                    fts_rank += batch.num_rows();
                }
                
                tracing::debug!("FTS search added candidates, total now: {}", candidates.len());

                // === Reciprocal Rank Fusion (RRF) with metadata boosts ===
                const RRF_K: f32 = 60.0;
                
                let mut scored_results: Vec<(f32, String, String)> = candidates
                    .into_values()
                    .map(|c| {
                        // Base RRF score from rank fusion
                        let rrf_score = Self::compute_rrf_score(c.vector_rank, c.fts_rank, RRF_K);
                        
                        // Metadata boosts
                        let mut boost = 0.0;
                        let query_lower = query.to_lowercase();
                        
                        if !c.service.is_empty() {
                            let service_singular = c.service.trim_end_matches('s');
                            if query_lower.contains(service_singular) || query_lower.contains(&c.service) {
                                boost += 0.01;
                            }
                        }
                        if !c.operation.is_empty() && query_lower.contains(&c.operation) {
                            boost += 0.01;
                        }
                        if !c.symbols.is_empty() && c.symbols.to_lowercase().contains(&query_lower) {
                            boost += 0.02;
                        }
                        
                        let final_score = rrf_score + boost;
                        
                        tracing::debug!(
                            "RRF: id='{}' vec_rank={:?} fts_rank={:?} rrf={:.4} boost={:.4} final={:.4}",
                            c.id, c.vector_rank, c.fts_rank, rrf_score, boost, final_score
                        );
                        
                        (final_score, c.text, c.source_file)
                    })
                    .collect();

                // Sort by final score (descending)
                scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored_results.truncate(limit);

                let search_results: Vec<DocSearchResult> = scored_results
                    .into_iter()
                    .map(|(score, text, source_file)| DocSearchResult {
                        text,
                        source_file,
                        score,
                    })
                    .collect();

                tracing::info!(
                    "Hybrid search for '{}': {} results (from {} vector + FTS candidates)", 
                    query, search_results.len(), candidate_pool
                );

                Ok(search_results)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_chunk_text_basic() {
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .expect("Failed to load tokenizer");
        
        let text = "This is a test document about clusters API. The create method allows you to create new clusters.";
        let chunks = chunk_text(
            text, 
            &tokenizer, 
            "test.rst", 
            "clusters", 
            "ClustersAPI", 
            "create",
            "clusters create ClustersAPI"
        ).expect("Chunking failed");
        
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
    fn test_embedder_initialization_performance() {
        let start = Instant::now();
        let embedder = Embedder::new().expect("Failed to create embedder");
        let init_time = start.elapsed();
        
        println!("Embedder initialization took {:?}", init_time);
        
        // Should initialize in under 1 second
        assert!(init_time.as_secs() < 1, "Embedder init took too long: {:?}", init_time);
        
        // Test single embedding
        let embed_start = Instant::now();
        let embedding = embedder.embed("test query").expect("Embedding failed");
        let embed_time = embed_start.elapsed();
        
        println!("Single embedding took {:?}", embed_time);
        assert_eq!(embedding.len(), EMBEDDING_DIM);
    }

    #[test]
    fn test_batch_embedding_performance() {
        let embedder = Embedder::new().expect("Failed to create embedder");
        
        // Create 100 test texts
        let texts: Vec<String> = (0..100)
            .map(|i| format!("This is test document number {} about various SDK operations like create, list, and delete.", i))
            .collect();
        
        let start = Instant::now();
        let embeddings = embedder.embed_batch(&texts).expect("Batch embedding failed");
        let elapsed = start.elapsed();
        
        println!("Batch embedding {} texts took {:?}", texts.len(), elapsed);
        println!("Average time per text: {:?}", elapsed / texts.len() as u32);
        
        assert_eq!(embeddings.len(), 100);
        for emb in &embeddings {
            assert_eq!(emb.len(), EMBEDDING_DIM);
        }
        
        // Should process 100 texts in under 5 seconds
        assert!(elapsed.as_secs() < 5, "Batch embedding took too long: {:?}", elapsed);
    }

    #[tokio::test]
    async fn test_full_sdk_indexing_performance() {
        use tempfile::TempDir;
        use tracing_subscriber::EnvFilter;
        
        // Initialize tracing for test output
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env().add_directive("_core=debug".parse().unwrap()))
            .with_test_writer()
            .try_init();
        
        // Use constant version for testing to avoid Python interop issues in unit tests
        let version = "0.80.0";
        
        println!("Testing full SDK indexing for version: {}", version);
        
        // Create temporary database directory
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test_db");
        
        // Create index with temp db path
        let mut index = SDKDocsIndex::with_db_path(db_path).expect("Failed to create index");
        
        // Measure full bootstrap time (download + index)
        let start = Instant::now();
        let result = index.bootstrap_with_version(&SDKSource::DatabricksSdkPython, &version).await;
        let elapsed = start.elapsed();
        
        match result {
            Ok(_) => {
                println!("Full SDK indexing completed in {:?}", elapsed);
                println!("  - File loading + parsing");
                println!("  - Text chunking (parallel)");
                println!("  - Embedding generation (parallel tokenization/post-processing)");
                println!("  - LanceDB table creation + FTS index");
                
                // Assert it completes under 10 seconds
                assert!(
                    elapsed.as_secs() < 10,
                    "Full SDK indexing took too long: {:?} (expected < 10s)",
                    elapsed
                );
            }
            Err(e) => {
                // If it fails due to missing cached SDK docs, that's acceptable for this test
                if e.contains("Failed to download") || e.contains("not cached") {
                    println!("Skipping test: SDK docs not cached and download failed: {}", e);
                } else {
                    panic!("SDK indexing failed: {}", e);
                }
            }
        }
    }
}
