/// Embedded model files for all-MiniLM-L6-v2 sentence transformer.
///
/// These files are embedded at compile time from assets/models/all-MiniLM-L6-v2/
/// The model produces 384-dimensional embeddings for semantic search.

/// Tokenizer configuration (JSON format)
pub const TOKENIZER_JSON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/all-MiniLM-L6-v2/tokenizer.json"
));

/// Model configuration (JSON format)
pub const CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/all-MiniLM-L6-v2/config.json"
));

/// Model weights in safetensors format
pub const MODEL_SAFETENSORS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/all-MiniLM-L6-v2/model.safetensors"
));

/// Model metadata
pub const EMBEDDING_DIM: usize = 384;
pub const MAX_SEQ_LENGTH: usize = 256;