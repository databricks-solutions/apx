use lancedb::{Connection, Table};
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use arrow::array::{Float32Array, StringArray, FixedSizeListArray, ArrayRef};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::{RecordBatch, RecordBatchIterator};
use tokenizers::Tokenizer;

use crate::search::embedder::Embedder;
use crate::search::embedded_model::EMBEDDING_DIM;
use crate::search::common;
use crate::interop::get_databricks_sdk_version;

const GITHUB_REPO: &str = "databricks/databricks-sdk-py";
const CHUNK_SIZE: usize = 256; // tokens
const CHUNK_OVERLAP: usize = 64; // 25% of 256

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
}

/// Search result with score
#[derive(Debug, Clone, Serialize)]
pub struct DocSearchResult {
    pub text: String,
    pub source_file: String,
    pub score: f32,
}

/// SDK documentation source enum
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SDKSource {
    #[serde(rename = "databricks-sdk-python")]
    DatabricksSdkPython,
}

/// Get cache path for SDK documentation
fn get_cache_path(version: &str) -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".apx")
        .join("cache")
        .join("databricks-sdk")
        .join(version)
}

/// Check if docs are cached for this version
fn is_cached(version: &str) -> bool {
    let cache_path = get_cache_path(version);
    let docs_path = cache_path.join("docs");
    docs_path.exists() && docs_path.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// Get GitHub zipball URL for a specific version
fn get_github_zipball_url(version: &str) -> String {
    format!("https://github.com/{}/archive/refs/tags/v{}.zip", GITHUB_REPO, version)
}

/// Download and extract SDK repository
async fn download_and_extract_sdk(version: &str) -> Result<PathBuf, String> {
    let cache_path = get_cache_path(version);
    let docs_path = cache_path.join("docs");

    if is_cached(version) {
        tracing::debug!("SDK docs already cached at {:?}", docs_path);
        return Ok(docs_path);
    }

    tracing::info!("Downloading Databricks SDK v{} from GitHub", version);
    let url = get_github_zipball_url(version);

    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to download SDK: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Failed to download SDK: HTTP {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Extract ZIP
    tracing::info!("Extracting SDK archive");
    fs::create_dir_all(&cache_path)
        .map_err(|e| format!("Failed to create cache directory: {}", e))?;

    let cursor = Cursor::new(bytes.to_vec());
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| format!("Failed to read ZIP archive: {}", e))?;

    // Find root folder name
    let root_folder = if archive.len() > 0 {
        let first_file = archive.by_index(0)
            .map_err(|e| format!("Failed to read first file: {}", e))?;
        let name = first_file.name();
        name.split('/').next().unwrap_or("").to_string()
    } else {
        return Err("Empty ZIP archive".to_string());
    };

    // Extract docs/ folder
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)
            .map_err(|e| format!("Failed to read file at index {}: {}", i, e))?;

        let file_path = file.name().to_string();

        // Only extract docs/ folder
        if !file_path.starts_with(&format!("{}/docs/", root_folder)) {
            continue;
        }

        let relative_path = file_path.strip_prefix(&format!("{}/", root_folder)).unwrap();
        let target_path = cache_path.join(relative_path);

        if file.is_dir() {
            fs::create_dir_all(&target_path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        } else {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }

            let mut outfile = fs::File::create(&target_path)
                .map_err(|e| format!("Failed to create file: {}", e))?;

            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("Failed to write file: {}", e))?;
        }
    }

    tracing::info!("SDK docs extracted to {:?}", docs_path);
    Ok(docs_path)
}

/// Simple RST to text converter
/// Strips RST directives and formatting, keeping the content
fn rst_to_text(rst_content: &str) -> String {
    let mut lines = Vec::new();

    for line in rst_content.lines() {
        let trimmed = line.trim();

        // Skip RST directives
        if trimmed.starts_with(".. ") || trimmed.starts_with(":") {
            continue;
        }

        // Skip header underlines (===, ---, ~~~, etc.)
        if trimmed.chars().all(|c| c == '=' || c == '-' || c == '~' || c == '^' || c == '*') {
            continue;
        }

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        lines.push(trimmed.to_string());
    }

    lines.join(" ")
}

/// Load all RST files from docs/workspace/
fn load_rst_files(docs_path: &Path) -> Result<Vec<(String, String)>, String> {
    let workspace_path = docs_path.join("workspace");

    if !workspace_path.exists() {
        return Err(format!("docs/workspace not found at {:?}", workspace_path));
    }

    let mut files = Vec::new();

    for entry in walkdir::WalkDir::new(&workspace_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Only process .rst files, skip index.rst
        if path.extension().and_then(|s| s.to_str()) == Some("rst")
            && path.file_stem().and_then(|s| s.to_str()) != Some("index") {

            let content = fs::read_to_string(path)
                .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;

            let text = rst_to_text(&content);

            if !text.is_empty() {
                let relative_path = path.strip_prefix(docs_path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                files.push((relative_path, text));
            }
        }
    }

    Ok(files)
}

/// Chunk text into overlapping segments based on tokens
fn chunk_text(text: &str, tokenizer: &Tokenizer, file_path: &str) -> Result<Vec<(String, String, usize)>, String> {
    let encoding = tokenizer.encode(text, false)
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
            text.len()
        };

        let chunk_text = &text[start_offset..end_offset];

        if !chunk_text.trim().is_empty() {
            let chunk_id = format!("{}:{}", file_path, chunk_index);
            chunks.push((chunk_id, chunk_text.to_string(), chunk_index));
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

    // Create record batch
    RecordBatch::try_new(
        std::sync::Arc::new(schema),
        vec![
            std::sync::Arc::new(id_array) as ArrayRef,
            std::sync::Arc::new(text_array) as ArrayRef,
            std::sync::Arc::new(source_file_array) as ArrayRef,
            std::sync::Arc::new(chunk_index_array) as ArrayRef,
            std::sync::Arc::new(embedding_array) as ArrayRef,
        ],
    )
    .map_err(|e| format!("Failed to create record batch: {}", e))
}

/// SDK documentation index using LanceDB
pub struct SDKDocIndex {
    db_path: PathBuf,
    embedder: Arc<Embedder>,
    tokenizer: Arc<Tokenizer>,
    version: Option<String>,
}

impl SDKDocIndex {
    /// Create a new SDK doc index
    pub fn new() -> Result<Self, String> {
        let db_path = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?
            .join(".apx")
            .join("db");

        let embedder = Embedder::new()?;

        // Load tokenizer from embedded model
        let tokenizer = Tokenizer::from_bytes(crate::search::embedded_model::TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        Ok(Self {
            db_path,
            embedder: Arc::new(embedder),
            tokenizer: Arc::new(tokenizer),
            version: None,
        })
    }

    /// Get LanceDB connection
    async fn get_connection(&self) -> Result<Connection, String> {
        common::get_connection(&self.db_path).await
    }

    /// Check if the index table exists
    async fn table_exists(&self, table_name: &str) -> Result<bool, String> {
        common::table_exists(&self.db_path, table_name).await
    }

    /// Get table
    async fn get_table(&self, table_name: &str) -> Result<Table, String> {
        common::get_table(&self.db_path, table_name).await
    }

    /// Bootstrap: download docs and build index
    pub async fn bootstrap(&mut self, source: &SDKSource) -> Result<bool, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                // Get SDK version
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| "databricks-sdk is not installed".to_string())?;

                tracing::info!("Found Databricks SDK version: {}", version);
                self.version = Some(version.clone());

                let table_name = format!("sdk_docs_python_{}", version.replace('.', "_"));

                // Check if already indexed
                if self.table_exists(&table_name).await? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }

                // Download and extract
                let docs_path = download_and_extract_sdk(&version).await?;

                // Load RST files
                tracing::info!("Loading RST files from docs/workspace/");
                let files = load_rst_files(&docs_path)?;
                tracing::info!("Loaded {} RST files", files.len());

                // Chunk all files
                let mut all_chunks: Vec<(String, String, String, usize)> = Vec::new();
                for (file_path, text) in files {
                    let chunks = chunk_text(&text, &self.tokenizer, &file_path)?;
                    for (id, chunk_text, chunk_index) in chunks {
                        all_chunks.push((id, chunk_text, file_path.clone(), chunk_index));
                    }
                }

                tracing::info!("Created {} text chunks", all_chunks.len());

                // Generate embeddings
                tracing::info!("Generating embeddings for {} chunks", all_chunks.len());
                let texts: Vec<String> = all_chunks.iter().map(|(_, text, _, _)| text.clone()).collect();
                let embeddings = self.embedder.embed_batch(&texts)?;

                // Create doc chunks
                let doc_chunks: Vec<DocChunk> = all_chunks
                    .into_iter()
                    .zip(embeddings.into_iter())
                    .map(|((id, text, source_file, chunk_index), emb)| {
                        let emb_array: [f32; EMBEDDING_DIM] = emb.try_into()
                            .expect("Embedding should have correct length");

                        DocChunk {
                            id,
                            text,
                            source_file,
                            chunk_index,
                            embedding: emb_array,
                        }
                    })
                    .collect();

                // Create table
                let conn = self.get_connection().await?;

                // Drop existing table if it exists
                if self.table_exists(&table_name).await? {
                    tracing::debug!("Dropping existing table: {}", table_name);
                    conn.drop_table(&table_name, &[])
                        .await
                        .map_err(|e| format!("Failed to drop existing table: {}", e))?;
                }

                // Convert to Arrow format
                let batch = chunks_to_batch(doc_chunks)?;
                let schema = batch.schema();
                let batches = RecordBatchIterator::new(
                    vec![Ok(batch)].into_iter(),
                    schema,
                );

                conn.create_table(&table_name, Box::new(batches))
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to create table: {}", e))?;

                tracing::info!("SDK docs indexed successfully");
                Ok(true)
            }
        }
    }

    /// Search for relevant documentation chunks
    pub async fn search(
        &self,
        source: &SDKSource,
        query: &str,
        limit: usize,
    ) -> Result<Vec<DocSearchResult>, String> {
        match source {
            SDKSource::DatabricksSdkPython => {
                // Get SDK version
                let version = get_databricks_sdk_version()?
                    .ok_or_else(|| "databricks-sdk is not installed. Please install databricks-sdk to use this feature.".to_string())?;

                let table_name = format!("sdk_docs_python_{}", version.replace('.', "_"));

                // Check if indexed
                if !self.table_exists(&table_name).await? {
                    return Err(format!(
                        "SDK docs not indexed for version {}. Index will be built on next server start.",
                        version
                    ));
                }

                // Generate query embedding
                let query_embedding = self.embedder.embed(query)?;

                // Get table
                let table = self.get_table(&table_name).await?;

                // Perform vector search
                let mut results = table
                    .query()
                    .nearest_to(query_embedding)
                    .map_err(|e| format!("Failed to create query: {}", e))?
                    .limit(limit)
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to execute search: {}", e))?;

                // Parse results
                let mut search_results = Vec::new();

                use futures_util::StreamExt;
                while let Some(batch_result) = results.next().await {
                    let batch = batch_result.map_err(|e| format!("Failed to read batch: {}", e))?;

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

                    let distance_array = batch
                        .column_by_name("_distance")
                        .ok_or("Missing _distance column")?
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .ok_or("Failed to downcast distance column")?;

                    for i in 0..batch.num_rows() {
                        let text = text_array.value(i).to_string();
                        let source_file = source_file_array.value(i).to_string();
                        let distance = distance_array.value(i);
                        let score = 1.0 - distance;

                        search_results.push(DocSearchResult {
                            text,
                            source_file,
                            score,
                        });
                    }
                }

                tracing::info!("Found {} doc results for query: {}", search_results.len(), query);

                Ok(search_results)
            }
        }
    }
}
