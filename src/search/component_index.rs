use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::index::scalar::FullTextSearchQuery;
use std::path::PathBuf;
use arrow::array::{StringArray, ArrayRef, Float32Array, FixedSizeListArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};

use crate::cli::components::get_all_registry_indexes;
use crate::search::embedder::Embedder;
use crate::search::embedded_model::EMBEDDING_DIM;
use super::common;

const SCHEMA_VERSION: u32 = 4; // v4: Added vector embeddings for hybrid search

/// Component record for LanceDB storage (Hybrid: Vector + FTS)
#[derive(Debug, Clone)]
pub struct ComponentRecord {
    /// Component ID: either "component-name" or "@registry-name/component-name"
    pub id: String,
    /// Component name
    pub name: String,
    /// Registry name (empty for default shadcn/ui)
    pub registry: String,
    /// Full searchable text (name + description)
    pub text: String,
    /// Embedding vector for semantic search
    pub embedding: [f32; EMBEDDING_DIM],
}

/// Search result with component details
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub name: String,
    pub registry: String,
    pub score: f32,
}

/// Component search index using LanceDB FTS
pub struct ComponentIndex {
    db_path: PathBuf,
}

/// Helper function to convert records to Arrow RecordBatch
fn records_to_batch(records: Vec<ComponentRecord>) -> Result<RecordBatch, String> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("registry", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
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
    ]);

    let id_array = StringArray::from(
        records.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
    );
    let name_array = StringArray::from(
        records.iter().map(|r| r.name.as_str()).collect::<Vec<_>>()
    );
    let registry_array = StringArray::from(
        records.iter().map(|r| r.registry.as_str()).collect::<Vec<_>>()
    );
    let text_array = StringArray::from(
        records.iter().map(|r| r.text.as_str()).collect::<Vec<_>>()
    );

    // Create embedding array
    let mut embedding_values: Vec<f32> = Vec::with_capacity(records.len() * EMBEDDING_DIM);
    for record in &records {
        embedding_values.extend_from_slice(&record.embedding);
    }

    let values = Float32Array::from(embedding_values);
    let field = Field::new("item", DataType::Float32, true);
    let embedding_array = FixedSizeListArray::try_new(
        std::sync::Arc::new(field),
        EMBEDDING_DIM as i32,
        std::sync::Arc::new(values),
        None,
    ).map_err(|e| format!("Failed to create fixed size list array: {e}"))?;

    RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(id_array) as ArrayRef,
            std::sync::Arc::new(name_array) as ArrayRef,
            std::sync::Arc::new(registry_array) as ArrayRef,
            std::sync::Arc::new(text_array) as ArrayRef,
            std::sync::Arc::new(embedding_array) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to create record batch: {e}"))
}

impl ComponentIndex {
    /// Create a new component index
    pub fn new(db_path: PathBuf) -> Result<Self, String> {
        Ok(Self { db_path })
    }

    /// Get the default index path (~/.apx/db/)
    pub fn default_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?;
        Ok(home.join(".apx").join("db"))
    }

    /// Get table name with schema version
    pub fn table_name(base_name: &str) -> String {
        format!("{}_fts_v{}", base_name, SCHEMA_VERSION)
    }

    /// Check if the index table exists (internal)
    async fn table_exists(&self, table_name: &str) -> Result<bool, String> {
        common::table_exists(&self.db_path, table_name).await
    }

    /// Validate that the index is usable by attempting a count query
    /// Returns Ok(true) if valid, Ok(false) if table doesn't exist, Err if corrupted
    pub async fn validate_index(&self, table_name: &str) -> Result<bool, String> {
        use futures_util::StreamExt;
        
        if !self.table_exists(table_name).await? {
            return Ok(false);
        }

        // Try to open the table and do a simple query to verify data files exist
        let table = match common::get_table(&self.db_path, table_name).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Failed to open table {}: {}", table_name, e);
                return Err(format!("Index corrupted: {}", e));
            }
        };

        // Try to count rows - this will fail if data files are missing
        let mut stream = table
            .query()
            .limit(1)
            .execute()
            .await
            .map_err(|e| format!("Index validation failed: {}", e))?;

        // Try to read at least one batch to verify data accessibility
        match stream.next().await {
            Some(Ok(_)) => Ok(true),
            Some(Err(e)) => Err(format!("Index data corrupted: {}", e)),
            None => Ok(true), // Empty table is valid
        }
    }

    /// Build index from registry.json files with embeddings for hybrid search
    pub async fn build_index_from_registries(&self, table_name: &str, embedder: &Embedder) -> Result<(), String> {
        tracing::info!("Building component hybrid index from registry indexes");

        // Load all registry indexes
        let all_indexes = get_all_registry_indexes()
            .map_err(|e| format!("Failed to load registry indexes: {}", e))?;

        if all_indexes.is_empty() {
            tracing::warn!("No registry indexes found. Index will be empty.");
            return Ok(());
        }

        // Convert to records with enriched text
        let mut records_data: Vec<(String, String, String, String)> = Vec::new();

        for (registry_name, items) in all_indexes {
            let is_default = registry_name == "ui";

            for item in items {
                let (id, registry) = if is_default {
                    (item.name.clone(), String::new())
                } else {
                    (format!("@{}/{}", registry_name, item.name), registry_name.clone())
                };

                // Enrich text for better semantic search
                let text = match &item.description {
                    Some(desc) if !desc.is_empty() => 
                        format!("{} {} ui component shadcn", item.name, desc),
                    _ => format!("{} ui component shadcn", item.name),
                };

                records_data.push((id, item.name, registry, text));
            }
        }

        tracing::info!("Generating embeddings for {} components", records_data.len());

        // Generate embeddings for all texts
        let embed_start = std::time::Instant::now();
        let texts: Vec<String> = records_data.iter().map(|(_, _, _, text)| text.clone()).collect();
        let embeddings = embedder.embed_batch(&texts)?;
        tracing::info!("Embedding generation took {:?}", embed_start.elapsed());

        // Create final records with embeddings
        let records_start = std::time::Instant::now();
        let records: Vec<ComponentRecord> = records_data
            .into_iter()
            .zip(embeddings.into_iter())
            .map(|((id, name, registry, text), emb)| {
                let embedding: [f32; EMBEDDING_DIM] = emb.try_into()
                    .expect("Embedding should have correct length");
                ComponentRecord { id, name, registry, text, embedding }
            })
            .collect();
        tracing::debug!("Record creation took {:?}", records_start.elapsed());

        tracing::info!("Indexing {} components with embeddings", records.len());

        // Create table
        let conn = common::get_connection(&self.db_path).await?;

        // Drop existing table
        if self.table_exists(table_name).await? {
            conn.drop_table(table_name, &[])
                .await
                .map_err(|e| format!("Failed to drop existing table: {e}"))?;
        }

        let batch = records_to_batch(records)?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);

        let table = conn.create_table(table_name, Box::new(batches))
            .execute()
            .await
            .map_err(|e| format!("Failed to create table: {e}"))?;

        // Create FTS index on text column for hybrid search
        tracing::info!("Creating FTS index on text column");
        table.create_index(
            &["text"],
            lancedb::index::Index::FTS(lancedb::index::scalar::FtsIndexBuilder::default())
        )
            .execute()
            .await
            .map_err(|e| format!("Failed to create FTS index: {e}"))?;

        tracing::info!("Component hybrid index built successfully (vector + FTS)");
        Ok(())
    }

    /// Search for components using hybrid vector + FTS search with RRF
    pub async fn search(
        &self,
        table_name: &str,
        query: &str,
        limit: usize,
        embedder: &Embedder,
    ) -> Result<Vec<SearchResult>, String> {
        use std::collections::HashMap;
        use futures_util::StreamExt;
        use crate::search::hybrid::{HybridSearchConfig, compute_rrf_score};

        if !self.table_exists(table_name).await? {
            return Err("Index not built. Please ensure components are indexed.".to_string());
        }

        let table = common::get_table(&self.db_path, table_name).await?;
        
        // Configuration for hybrid search
        let config = HybridSearchConfig::default();
        let candidate_pool = (limit * config.candidate_pool_multiplier).max(50);

        // Embed query for vector search
        let query_embedding = embedder.embed(query)?;

        // === Vector Search ===
        let mut vector_results = table
            .query()
            .nearest_to(query_embedding)
            .map_err(|e| format!("Failed to create vector query: {}", e))?
            .limit(candidate_pool)
            .execute()
            .await
            .map_err(|e| format!("Failed to execute vector search: {}", e))?;

        // Candidate storage: id -> (name, registry, vector_rank, fts_rank)
        let mut candidates: HashMap<String, (String, String, Option<usize>, Option<usize>)> = HashMap::new();
        let mut rank = 0;

        while let Some(batch_result) = vector_results.next().await {
            let batch = batch_result.map_err(|e| format!("Failed to read vector batch: {}", e))?;

            let id_array = batch
                .column_by_name("id")
                .ok_or("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast id column")?;

            let name_array = batch
                .column_by_name("name")
                .ok_or("Missing name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast name column")?;

            let registry_array = batch
                .column_by_name("registry")
                .ok_or("Missing registry column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast registry column")?;

            for i in 0..batch.num_rows() {
                let id = id_array.value(i).to_string();
                let name = name_array.value(i).to_string();
                let registry = registry_array.value(i).to_string();
                
                candidates.insert(id, (name, registry, Some(rank + i), None));
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

            let id_array = batch
                .column_by_name("id")
                .ok_or("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast id column")?;

            let name_array = batch
                .column_by_name("name")
                .ok_or("Missing name column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast name column")?;

            let registry_array = batch
                .column_by_name("registry")
                .ok_or("Missing registry column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast registry column")?;

            for i in 0..batch.num_rows() {
                let id = id_array.value(i).to_string();
                let name = name_array.value(i).to_string();
                let registry = registry_array.value(i).to_string();
                let r = fts_rank + i;
                
                if let Some(existing) = candidates.get_mut(&id) {
                    // Already in vector results, add FTS rank
                    existing.3 = Some(r);
                } else {
                    // New candidate from FTS only
                    candidates.insert(id, (name, registry, None, Some(r)));
                }
            }
            fts_rank += batch.num_rows();
        }

        tracing::debug!("FTS search added candidates, total now: {}", candidates.len());

        // === RRF with Registry Boost ===
        let mut scored_results: Vec<(f32, String, String, String)> = candidates
            .into_iter()
            .map(|(id, (name, registry, vec_rank, fts_rank))| {
                // Base RRF score
                let rrf_score = compute_rrf_score(vec_rank, fts_rank, &config);
                
                // Registry boost: default (empty registry) gets priority
                let registry_boost = if registry.is_empty() {
                    0.02  // Boost default shadcn/ui registry
                } else {
                    0.0
                };
                
                let final_score = rrf_score + registry_boost;
                
                tracing::debug!(
                    "RRF: id='{}' vec_rank={:?} fts_rank={:?} rrf={:.4} boost={:.4} final={:.4}",
                    id, vec_rank, fts_rank, rrf_score, registry_boost, final_score
                );
                
                (final_score, id, name, registry)
            })
            .collect();

        // Sort by final score (descending)
        scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        
        // Normalize scores to 0-1 range for better interpretability
        // Top score becomes ~1.0, lowest becomes proportionally smaller
        let max_score = scored_results.first().map(|(s, _, _, _)| *s).unwrap_or(1.0);
        let min_score = scored_results.last().map(|(s, _, _, _)| *s).unwrap_or(0.0);
        let score_range = max_score - min_score;
        
        let mut normalized_results: Vec<(f32, String, String, String)> = scored_results
            .into_iter()
            .map(|(score, id, name, registry)| {
                // Normalize to 0-1 range, with top result close to 1.0
                let normalized = if score_range > 0.0 {
                    0.3 + 0.7 * ((score - min_score) / score_range)  // Range: [0.3, 1.0]
                } else {
                    1.0  // Single result or all same score
                };
                (normalized, id, name, registry)
            })
            .collect();
        
        normalized_results.truncate(limit);

        let search_results: Vec<SearchResult> = normalized_results
            .into_iter()
            .map(|(score, id, name, registry)| SearchResult {
                id,
                name,
                registry,
                score,
            })
            .collect();

        tracing::info!(
            "Hybrid search for '{}': {} results (from {} candidates)", 
            query, search_results.len(), candidate_pool
        );

        Ok(search_results)
    }
}
