//! Embedded model files for bge-small-en-v1.5 sentence transformer.
//!
//! These files are embedded at compile time from assets/models/bge-small-en-v1.5/
//! The model produces 384-dimensional embeddings for semantic search.

// Tokenizer configuration (JSON format)
pub const TOKENIZER_JSON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/bge-small-en-v1.5/tokenizer.json"
));

/// Model configuration (JSON format)
#[allow(dead_code)]
pub const CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/bge-small-en-v1.5/config.json"
));

/// Model weights in ONNX format
pub const MODEL_ONNX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/bge-small-en-v1.5/onnx_model.onnx"
));

/// Model metadata
pub const EMBEDDING_DIM: usize = 384;
pub const MAX_SEQ_LENGTH: usize = 512;