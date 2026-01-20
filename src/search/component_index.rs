use lancedb::{Connection, Table};
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use arrow::array::{
    Float32Array, StringArray, FixedSizeListArray, ArrayRef,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};

use crate::cli::components::{RegistryItem, cache};

use super::embedder::Embedder;
use super::embedded_model::EMBEDDING_DIM;
use super::common;

/// Minimal component record for LanceDB storage
/// Only contains ID and embeddings to minimize storage
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComponentRecord {
    /// Component ID: either "component-name" or "@registry-name/component-name"
    pub id: String,
    /// Embedding vector (384 dimensions for all-MiniLM-L6-v2)
    #[serde(with = "common::serde_arrays")]
    pub embedding: [f32; EMBEDDING_DIM],
}

/// Component with metadata for indexing
#[derive(Debug, Clone)]
pub struct ComponentMetadata {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub categories: Vec<String>,
}

impl ComponentMetadata {
    /// Create searchable text from component metadata
    pub fn to_searchable_text(&self) -> String {
        let mut parts = vec![self.name.clone()];

        if let Some(desc) = &self.description {
            parts.push(desc.clone());
        }

        if !self.categories.is_empty() {
            parts.push(self.categories.join(" "));
        }

        parts.join(" | ")
    }

    /// Create from registry item
    pub fn from_registry_item(item: &RegistryItem, registry: Option<&str>) -> Self {
        let id = match registry {
            Some(reg) => format!("@{}/{}", reg, item.name),
            None => item.name.clone(),
        };

        Self {
            id,
            name: item.name.clone(),
            description: item.description.clone(),
            categories: item.categories.clone(),
        }
    }
}

/// Search result with score
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
}

/// Component search index using LanceDB
pub struct ComponentIndex {
    db_path: PathBuf,
    embedder: Arc<Embedder>,
}

/// Helper function to convert ComponentRecord vector to Arrow RecordBatch
fn records_to_batch(records: Vec<ComponentRecord>) -> Result<RecordBatch, String> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
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

    // Create ID array
    let id_array = StringArray::from(
        records.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
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

    // Create record batch
    RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(id_array) as ArrayRef,
            std::sync::Arc::new(embedding_array) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to create record batch: {e}"))
}

impl ComponentIndex {
    /// Create a new component index
    /// db_path should point to ~/.apx/db/ directory
    pub fn new(db_path: PathBuf) -> Result<Self, String> {
        let embedder = Embedder::new()?;

        Ok(Self {
            db_path,
            embedder: Arc::new(embedder),
        })
    }

    /// Get the default index path (~/.apx/db/)
    pub fn default_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?;

        Ok(home.join(".apx").join("db"))
    }

    /// Get LanceDB connection
    async fn get_connection(&self) -> Result<Connection, String> {
        common::get_connection(&self.db_path).await
    }

    /// Check if the index table exists
    async fn table_exists(&self, table_name: &str) -> Result<bool, String> {
        common::table_exists(&self.db_path, table_name).await
    }

    /// Get or create the components table
    async fn get_table(&self, table_name: &str) -> Result<Table, String> {
        common::get_table(&self.db_path, table_name).await
    }

    /// Build index from cached components
    /// Should be called after sync_all_registries() to populate cache
    pub async fn build_index(&self, app_dir: &Path, table_name: &str) -> Result<(), String> {
        tracing::info!("Building component index from cached components");

        // First, sync all registries to populate the cache
        tracing::info!("Syncing all registries to populate cache");
        match cache::sync_all_registries(app_dir).await {
            Ok(synced) => {
                let total: usize = synced.values().map(|v| v.len()).sum();
                tracing::info!("Synced {} components from {} registries", total, synced.len());
            }
            Err(e) => {
                tracing::warn!("Failed to sync registries: {}. Will use existing cache.", e);
            }
        }

        // Load all cached components
        let all_cached = cache::get_all_cached_components()
            .map_err(|e| format!("Failed to load cached components: {}", e))?;

        if all_cached.is_empty() {
            tracing::warn!("No cached components found. Index will be empty.");
            return Ok(());
        }

        // Convert to ComponentMetadata
        let mut components_to_index: Vec<ComponentMetadata> = Vec::new();

        for (registry_name, components) in all_cached {
            let registry_arg = if registry_name == "default" {
                None
            } else {
                Some(registry_name.as_str())
            };

            for (_component_name, item) in components {
                let metadata = ComponentMetadata::from_registry_item(&item, registry_arg);
                components_to_index.push(metadata);
            }
        }

        tracing::info!(
            "Loaded {} components from cache for indexing",
            components_to_index.len()
        );

        // Generate embeddings for all components
        let texts: Vec<String> = components_to_index
            .iter()
            .map(|c| c.to_searchable_text())
            .collect();

        tracing::info!("Generating embeddings for {} components", texts.len());
        let embeddings = self.embedder.embed_batch(&texts)?;

        // Create records
        let records: Vec<ComponentRecord> = components_to_index
            .into_iter()
            .zip(embeddings.into_iter())
            .map(|(comp, emb)| {
                let emb_array: [f32; EMBEDDING_DIM] = emb.try_into()
                    .expect("Embedding should have correct length");

                ComponentRecord {
                    id: comp.id,
                    embedding: emb_array,
                }
            })
            .collect();

        // Create or replace table
        let conn = self.get_connection().await?;

        // Drop existing table if it exists
        if self.table_exists(table_name).await? {
            tracing::debug!("Dropping existing table: {}", table_name);
            conn.drop_table(table_name, &[])
                .await
                .map_err(|e| format!("Failed to drop existing table: {e}"))?;
        }

        // Convert records to Arrow format for LanceDB
        let batch = records_to_batch(records)?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(
            vec![Ok(batch)].into_iter(),
            schema,
        );

        conn.create_table(table_name, Box::new(batches))
            .execute()
            .await
            .map_err(|e| format!("Failed to create table: {e}"))?;

        tracing::info!("Component index built successfully");

        Ok(())
    }

    /// Search for components
    /// Returns top-k results with scores
    pub async fn search(
        &self,
        table_name: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        tracing::info!("Searching for: {}", query);

        // Check if table exists
        if !self.table_exists(table_name).await? {
            return Err("Index not built yet. Please ensure components are indexed.".to_string());
        }

        // Generate query embedding
        let query_embedding = self.embedder.embed(query)?;

        // Get table
        let table = self.get_table(table_name).await?;

        // Perform vector search
        let mut results = table
            .query()
            .nearest_to(query_embedding)
            .map_err(|e| format!("Failed to create query: {e}"))?
            .limit(limit)
            .execute()
            .await
            .map_err(|e| format!("Failed to execute search: {e}"))?;

        // Parse results
        let mut search_results = Vec::new();

        // Note: LanceDB returns RecordBatchStream, we need to extract id and distance
        use futures_util::StreamExt;
        while let Some(batch_result) = results.next().await {
            let batch = batch_result.map_err(|e| format!("Failed to read batch: {e}"))?;
            let id_array = batch
                .column_by_name("id")
                .ok_or("Missing id column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or("Failed to downcast id column")?;

            let distance_array = batch
                .column_by_name("_distance")
                .ok_or("Missing _distance column")?
                .as_any()
                .downcast_ref::<Float32Array>()
                .ok_or("Failed to downcast distance column")?;

            for i in 0..batch.num_rows() {
                let id = id_array.value(i).to_string();
                let distance = distance_array.value(i);
                // Convert distance to similarity score (cosine similarity = 1 - distance for normalized vectors)
                let score = 1.0 - distance;

                search_results.push(SearchResult { id, score });
            }
        }

        tracing::info!("Found {} results", search_results.len());

        Ok(search_results)
    }
}
