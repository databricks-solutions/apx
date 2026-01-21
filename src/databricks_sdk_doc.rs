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
const CHUNK_SIZE: usize = 512; // tokens - increased for more context
const CHUNK_OVERLAP: usize = 128; // tokens - 25% overlap for better coverage
const SCHEMA_VERSION: u32 = 5; // Increment when schema changes (v5: improved RST parsing with enums, attributes, code examples)

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

/// Extract method name from signature like "create(spark_version: str, ...)" -> "create"
fn extract_method_name(signature: &str) -> Option<&str> {
    let name_part = signature.split('(').next()?;
    // If it has a dot (e.g., "Class.method"), get the last part
    Some(name_part.split('.').last().unwrap_or(name_part))
}

/// Extract service name from class name (e.g., "ClustersAPI" -> "clusters")
fn extract_service_from_class(class_name: &str) -> Option<String> {
    let name = class_name
        .trim_end_matches("API")
        .trim_end_matches("Ext");
    
    if name.is_empty() {
        None
    } else {
        Some(name.to_lowercase())
    }
}

/// Parsed RST directive result
struct ParsedDirective {
    /// Text fragments to include in searchable text
    text_fragments: Vec<String>,
    /// Entity name (class)
    entity: Option<String>,
    /// Operation name (method)
    operation: Option<String>,
    /// Service name extracted from class
    service: Option<String>,
}

/// Parse a single RST directive line, returning extracted content
fn parse_rst_directive(directive_content: &str) -> Option<ParsedDirective> {
    let double_colon_pos = directive_content.find("::")?;
    let directive_type = &directive_content[..double_colon_pos];
    let directive_value = directive_content[double_colon_pos + 2..].trim();
    
    if directive_value.is_empty() && directive_type != "code-block" {
        return None;
    }
    
    let mut result = ParsedDirective {
        text_fragments: Vec::new(),
        entity: None,
        operation: None,
        service: None,
    };
    
    match directive_type {
        t if t.starts_with("py:class") => {
            result.text_fragments.push(directive_value.to_string());
            result.entity = Some(directive_value.to_string());
            if let Some(svc) = extract_service_from_class(directive_value) {
                result.text_fragments.push(svc.clone());
                result.service = Some(svc);
            }
        }
        t if t.starts_with("py:method") => {
            result.text_fragments.push(directive_value.to_string());
            if let Some(method_name) = extract_method_name(directive_value) {
                result.text_fragments.push(method_name.to_string());
                result.operation = Some(method_name.to_string());
            }
        }
        t if t.starts_with("py:attribute") => {
            result.text_fragments.push(format!("attribute {}", directive_value));
        }
        "autoclass" => {
            result.text_fragments.push(format!("class {}", directive_value));
        }
        t if t.starts_with("py:currentmodule") || t == "currentmodule" => {
            result.text_fragments.push(format!("module {}", directive_value));
        }
        "code-block" => {
            result.text_fragments.push("code example".to_string());
        }
        _ => return None,
    }
    
    Some(result)
}

/// Directive-aware RST to text converter
/// Returns (text, entity, operation, service, symbols)
fn parse_rst_content(rst_content: &str) -> (String, String, String, String, Vec<String>) {
    let mut output = Vec::new();
    let mut in_code_block = false;
    
    // Metadata collected from directives
    let mut entity = String::new();
    let mut operation = String::new();
    let mut service = String::new();
    let mut symbols = Vec::new();

    for line in rst_content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Handle code blocks
        if in_code_block {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_code_block = false;
                // Fall through to process this line
            } else {
                output.push(trimmed.to_string());
                continue;
            }
        }

        // Skip RST heading underlines (===, ---, ~~~, etc.)
        if trimmed.chars().all(|c| matches!(c, '=' | '-' | '~' | '^' | '*')) {
            continue;
        }

        // Process RST directives (lines starting with ".. ")
        if let Some(directive_content) = trimmed.strip_prefix(".. ") {
            if directive_content.starts_with("code-block::") {
                in_code_block = true;
                output.push("code example".to_string());
            } else if let Some(parsed) = parse_rst_directive(directive_content) {
                output.extend(parsed.text_fragments);
                
                // Collect metadata (keep first entity/service, collect all operations)
                if entity.is_empty() {
                    if let Some(e) = parsed.entity {
                        entity = e.clone();
                        symbols.push(e);
                    }
                }
                if let Some(s) = parsed.service {
                    if service.is_empty() {
                        service = s.clone();
                    }
                    symbols.push(s);
                }
                if let Some(op) = parsed.operation {
                    operation = op.clone(); // Keep last operation (methods come after class)
                    symbols.push(op);
                }
            }
            continue;
        }

        // Process field directives (:param:, :returns:, :value:, etc.)
        if trimmed.starts_with(':') {
            if let Some(colon_end) = trimmed[1..].find(':') {
                let field_name = &trimmed[1..=colon_end];
                let field_value = trimmed.get(colon_end + 2..).map(|s| s.trim()).unwrap_or("");
                
                let text = match field_name {
                    f if f.starts_with("param ") => {
                        let param_name = f.split_whitespace().nth(1).unwrap_or("");
                        if field_value.is_empty() {
                            format!("param {}", param_name)
                        } else {
                            format!("param {} {}", param_name, field_value)
                        }
                    }
                    f if f.starts_with("type ") => {
                        let type_name = f.split_whitespace().nth(1).unwrap_or("");
                        if field_value.is_empty() { continue; }
                        format!("type {} {}", type_name, field_value)
                    }
                    "returns" if !field_value.is_empty() => format!("returns {}", field_value),
                    "value" if !field_value.is_empty() => format!("value {}", field_value),
                    "members" | "undoc-members" => continue, // Skip directive options
                    _ => continue,
                };
                output.push(text);
            }
            continue;
        }

        // Preserve markdown-style links: [Link Text]: https://url
        if trimmed.starts_with('[') && trimmed.contains("]:") {
            output.push(trimmed.to_string());
            continue;
        }

        // Regular prose content (including heading text)
        output.push(trimmed.to_string());
    }

    (output.join(" "), entity, operation, service, symbols)
}

/// Extract metadata from file path and RST content
fn extract_metadata(file_path: &str, rst_content: &str) -> (String, String, String, String, String) {
    let (text, entity, operation, mut service, mut symbols) = parse_rst_content(rst_content);
    
    // Fallback: extract service from file path if not found in content
    if service.is_empty() {
        if let Some(stem) = Path::new(file_path).file_stem() {
            service = stem.to_string_lossy().to_lowercase();
        }
    }
    
    // Add service to symbols for matching
    if !service.is_empty() && !symbols.contains(&service) {
        symbols.push(service.clone());
    }

    (text, service, entity, operation, symbols.join(" "))
}

/// Simple markdown to text converter
fn md_to_text(md_content: &str) -> String {
    let mut output = Vec::new();
    let mut in_code_block = false;

    for line in md_content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            output.push(trimmed.to_string());
            continue;
        }

        // Remove markdown heading markers but keep text
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                output.push(heading.to_string());
            }
            continue;
        }

        output.push(trimmed.to_string());
    }

    output.join(" ")
}

/// Parsed documentation file
struct ParsedDocFile {
    relative_path: String,
    text: String,
    service: String,
    entity: String,
    operation: String,
    symbols: String,
}

/// Load RST files from a directory (recursive), skipping index.rst
fn load_rst_from_dir(
    dir_path: &Path, 
    docs_path: &Path,
    files: &mut Vec<ParsedDocFile>,
) -> Result<(), String> {
    if !dir_path.exists() {
        return Ok(());
    }
    
    for entry in walkdir::WalkDir::new(dir_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        
        // Only .rst files, skip index.rst
        if path.extension().and_then(|s| s.to_str()) != Some("rst") {
            continue;
        }
        if path.file_stem().and_then(|s| s.to_str()) == Some("index") {
            continue;
        }

        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;

        let relative_path = path.strip_prefix(docs_path)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let (text, service, entity, operation, symbols) = extract_metadata(&relative_path, &content);

        if !text.is_empty() {
            files.push(ParsedDocFile {
                relative_path,
                text,
                service,
                entity,
                operation,
                symbols,
            });
        }
    }
    
    Ok(())
}

/// Load all documentation files (RST from workspace/dbdataclasses, MD from root)
fn load_doc_files(docs_path: &Path) -> Result<Vec<ParsedDocFile>, String> {
    let mut files = Vec::new();

    // Load RST from workspace/ and dbdataclasses/
    load_rst_from_dir(&docs_path.join("workspace"), docs_path, &mut files)?;
    load_rst_from_dir(&docs_path.join("dbdataclasses"), docs_path, &mut files)?;

    // Load markdown files from docs root
    for entry in fs::read_dir(docs_path)
        .map_err(|e| format!("Failed to read docs directory: {}", e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();

        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;

        let text = md_to_text(&content);
        if text.is_empty() {
            continue;
        }

        let relative_path = path.strip_prefix(docs_path)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let file_stem = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        files.push(ParsedDocFile {
            relative_path,
            text,
            service: file_stem.clone(),
            entity: String::new(),
            operation: String::new(),
            symbols: format!("{} guide documentation", file_stem),
        });
    }

    if files.is_empty() {
        return Err("No documentation files found".to_string());
    }

    Ok(files)
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

                // Include schema version in table name to force rebuild when schema changes
                let table_name = format!("sdk_docs_python_{}_schema_v{}", version.replace('.', "_"), SCHEMA_VERSION);

                // Check if already indexed
                if self.table_exists(&table_name).await? {
                    tracing::info!("SDK docs already indexed for version {}", version);
                    return Ok(false);
                }

                // Download and extract
                let docs_path = download_and_extract_sdk(&version).await?;

                // Load documentation files
                tracing::info!("Loading documentation files from docs/workspace/, docs/dbdataclasses/, and docs/*.md");
                let files = load_doc_files(&docs_path)?;
                tracing::info!("Loaded {} documentation files", files.len());

                // Chunk all files
                let mut all_chunks: Vec<(String, String, String, usize, String, String, String, String)> = Vec::new();
                for (i, doc) in files.iter().enumerate() {
                    // Log first file to verify metadata extraction
                    if i == 0 {
                        tracing::info!(
                            "Sample metadata: file='{}', service='{}', entity='{}', operation='{}', symbols='{}'",
                            doc.relative_path, doc.service, doc.entity, doc.operation, doc.symbols
                        );
                    }
                    
                    let chunks = chunk_text(
                        &doc.text, 
                        &self.tokenizer, 
                        &doc.relative_path, 
                        &doc.service, 
                        &doc.entity, 
                        &doc.operation, 
                        &doc.symbols
                    )?;
                    
                    for (id, chunk_text, chunk_index, svc, ent, op, syms) in chunks {
                        all_chunks.push((id, chunk_text, doc.relative_path.clone(), chunk_index, svc, ent, op, syms));
                    }
                }

                tracing::info!("Created {} text chunks", all_chunks.len());

                // Generate embeddings
                tracing::info!("Generating embeddings for {} chunks", all_chunks.len());
                let texts: Vec<String> = all_chunks.iter().map(|(_, text, _, _, _, _, _, _)| text.clone()).collect();
                let embeddings = self.embedder.embed_batch(&texts)?;

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

    /// Tokenize text for lexical matching
    fn tokenize_for_lexical(text: &str) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect::<HashSet<String>>()
    }

    /// Compute lexical score based on token overlap and phrase matching
    fn compute_lexical_score(query: &str, text: &str) -> f32 {
        let query_tokens = Self::tokenize_for_lexical(query);
        let text_tokens = Self::tokenize_for_lexical(text);

        if query_tokens.is_empty() || text_tokens.is_empty() {
            return 0.0;
        }

        // Token overlap (Jaccard-like similarity)
        let overlap = query_tokens.intersection(&text_tokens).count();
        let union = query_tokens.union(&text_tokens).count();
        let token_score = overlap as f32 / union.max(1) as f32;

        // Exact phrase match bonus
        let phrase_bonus = if text.to_lowercase().contains(&query.to_lowercase()) {
            0.3
        } else {
            0.0
        };

        token_score + phrase_bonus
    }

    /// Compute final combined score with metadata boosts
    fn compute_final_score(
        vector_score: f32,
        query: &str,
        text: &str,
        service: &str,
        operation: &str,
        symbols: &str,
    ) -> f32 {
        let lexical = Self::compute_lexical_score(query, text);

        let mut boost = 0.0;
        let query_lower = query.to_lowercase();

        // Service match boost (e.g., "cluster" in query, service="clusters")
        if !service.is_empty() {
            let service_singular = service.trim_end_matches('s');
            if query_lower.contains(service_singular) || query_lower.contains(service) {
                boost += 0.3;
            }
        }

        // Operation match boost (e.g., "create" in query, operation="create")
        if !operation.is_empty() && query_lower.contains(operation) {
            boost += 0.3;
        }

        // Symbol exact match (big boost)
        if !symbols.is_empty() && symbols.to_lowercase().contains(&query_lower) {
            boost += 0.5;
        }

        // Weighted combination with normalized vector score
        // vector_score is already 1.0 - distance, which can be negative if distance > 1
        // Normalize to 0-1 range and combine with lexical and boosts
        let normalized_vector = (vector_score + 1.0) / 2.0; // Map [-1, 1] to [0, 1]
        
        let final_score = 0.4 * normalized_vector + 0.2 * lexical + boost;
        
        // Debug logging
        tracing::debug!(
            "Scoring: query='{}', service='{}', operation='{}', symbols='{}' | vec={:.3}, lex={:.3}, boost={:.3} -> final={:.3}",
            query, service, operation, symbols, normalized_vector, lexical, boost, final_score
        );
        
        final_score
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

                // Include schema version in table name (must match bootstrap)
                let table_name = format!("sdk_docs_python_{}_schema_v{}", version.replace('.', "_"), SCHEMA_VERSION);

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

                // Fetch larger candidate pool for reranking (10x the limit, minimum 100)
                let candidate_pool = (limit * 10).max(100);

                // Perform vector search with larger pool
                let mut results = table
                    .query()
                    .nearest_to(query_embedding)
                    .map_err(|e| format!("Failed to create query: {}", e))?
                    .limit(candidate_pool)
                    .execute()
                    .await
                    .map_err(|e| format!("Failed to execute search: {}", e))?;

                // Collect candidates with metadata for reranking
                let mut candidates: Vec<(f32, String, String, String, String, String)> = Vec::new();

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
                        let text = text_array.value(i).to_string();
                        let source_file = source_file_array.value(i).to_string();
                        let distance = distance_array.value(i);
                        let vector_score = 1.0 - distance;
                        let service = service_array.value(i).to_string();
                        let operation = operation_array.value(i).to_string();
                        let symbols = symbols_array.value(i).to_string();

                        candidates.push((vector_score, text, source_file, service, operation, symbols));
                    }
                }

                // Rerank candidates with combined score
                let mut scored_results: Vec<(f32, String, String)> = candidates
                    .into_iter()
                    .map(|(vector_score, text, source_file, service, operation, symbols)| {
                        let final_score = Self::compute_final_score(
                            vector_score,
                            query,
                            &text,
                            &service,
                            &operation,
                            &symbols,
                        );
                        (final_score, text, source_file)
                    })
                    .collect();

                // Sort by final score (descending)
                scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

                // Take top results
                scored_results.truncate(limit);

                // Convert to search results
                let search_results: Vec<DocSearchResult> = scored_results
                    .into_iter()
                    .map(|(score, text, source_file)| DocSearchResult {
                        text,
                        source_file,
                        score,
                    })
                    .collect();

                tracing::info!("Found {} doc results for query: {} (reranked from {} candidates)", 
                    search_results.len(), query, candidate_pool);

                Ok(search_results)
            }
        }
    }
}
