#![allow(dead_code)] // TODO: Remove once functionality is implemented

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

/// Optional: Tokenizer config (if needed)
pub const TOKENIZER_CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/all-MiniLM-L6-v2/tokenizer_config.json"
));

/// Optional: Vocabulary file (if needed)
pub const VOCAB_TXT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/models/all-MiniLM-L6-v2/vocab.txt"
));

/// Model metadata
pub const EMBEDDING_DIM: usize = 384;
pub const MAX_SEQ_LENGTH: usize = 256;
pub const MODEL_NAME: &str = "all-MiniLM-L6-v2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_files_not_empty() {
        assert!(!TOKENIZER_JSON.is_empty(), "Tokenizer JSON should not be empty");
        assert!(!CONFIG_JSON.is_empty(), "Config JSON should not be empty");
        assert!(!MODEL_SAFETENSORS.is_empty(), "Model safetensors should not be empty");
    }

    #[test]
    fn test_model_size_reasonable() {
        // all-MiniLM-L6-v2 should be around 22MB
        let model_size_mb = MODEL_SAFETENSORS.len() / (1024 * 1024);
        assert!(
            model_size_mb > 10 && model_size_mb < 50,
            "Model size should be reasonable (10-50 MB), got {} MB",
            model_size_mb
        );
    }
}
