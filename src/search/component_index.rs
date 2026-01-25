//! Component registry indexing and search using LanceDB with Full-Text Search.

use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::index::scalar::FullTextSearchQuery;
use std::path::PathBuf;
use arrow::array::{StringArray, ArrayRef};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};
use futures_util::StreamExt;

use crate::cli::components::get_all_registry_indexes;
use super::common;

const SCHEMA_VERSION: u32 = 1; // v1: Pure FTS (no embeddings)

/// Component record for LanceDB storage (FTS only)
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

    RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(id_array) as ArrayRef,
            std::sync::Arc::new(name_array) as ArrayRef,
            std::sync::Arc::new(registry_array) as ArrayRef,
            std::sync::Arc::new(text_array) as ArrayRef,
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

    /// Build index from registry.json files using Full-Text Search
    pub async fn build_index_from_registries(&self, table_name: &str) -> Result<(), String> {
        tracing::info!("Building component FTS index from registry indexes");

        // Load all registry indexes
        let all_indexes = get_all_registry_indexes()
            .map_err(|e| format!("Failed to load registry indexes: {}", e))?;

        if all_indexes.is_empty() {
            tracing::warn!("No registry indexes found. Index will be empty.");
            return Ok(());
        }

        // Convert to records with enriched text
        let mut records: Vec<ComponentRecord> = Vec::new();

        for (registry_name, items) in all_indexes {
            let is_default = registry_name == "ui";

            for item in items {
                let (id, registry) = if is_default {
                    (item.name.clone(), String::new())
                } else {
                    (format!("@{}/{}", registry_name, item.name), registry_name.clone())
                };

                // Enrich text for better FTS
                let text = match &item.description {
                    Some(desc) if !desc.is_empty() => 
                        format!("{} {} ui component shadcn", item.name, desc),
                    _ => format!("{} ui component shadcn", item.name),
                };

                records.push(ComponentRecord { id, name: item.name, registry, text });
            }
        }

        tracing::info!("Indexing {} components", records.len());

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

        // Create FTS index on text column
        tracing::info!("Creating FTS index on text column");
        table.create_index(
            &["text"],
            lancedb::index::Index::FTS(lancedb::index::scalar::FtsIndexBuilder::default())
        )
            .execute()
            .await
            .map_err(|e| format!("Failed to create FTS index: {e}"))?;

        tracing::info!("Component FTS index built successfully");
        Ok(())
    }

    /// Search for components using Full-Text Search
    pub async fn search(
        &self,
        table_name: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        if !self.table_exists(table_name).await? {
            return Err("Index not built. Please ensure components are indexed.".to_string());
        }

        let table = common::get_table(&self.db_path, table_name).await?;

        // Execute FTS search
        let fts_query = FullTextSearchQuery::new(query.to_string());
        let mut fts_results = table
            .query()
            .full_text_search(fts_query)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| format!("Failed to execute FTS search: {}", e))?;

        let mut results = Vec::new();
        let mut rank = 0;

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
                // Simple rank-based scoring (higher rank = lower score)
                // Boost default registry results
                let registry = registry_array.value(i).to_string();
                let registry_boost = if registry.is_empty() { 0.1 } else { 0.0 };
                let score = (1.0 / (1.0 + rank as f32)) + registry_boost;
                
                results.push(SearchResult {
                    id: id_array.value(i).to_string(),
                    name: name_array.value(i).to_string(),
                    registry,
                    score,
                });
                rank += 1;
            }
        }

        // Re-sort by score after applying registry boost
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        tracing::info!(
            "FTS search for '{}': {} results", 
            query, results.len()
        );

        Ok(results)
    }
}
