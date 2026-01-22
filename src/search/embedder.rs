use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use safetensors::SafeTensors;
use tokenizers::Tokenizer;
use std::sync::{Arc, Mutex};

use super::embedded_model::{
    CONFIG_JSON, MODEL_SAFETENSORS, TOKENIZER_JSON, EMBEDDING_DIM, MAX_SEQ_LENGTH,
};

/// Thread-safe embedding generator using all-MiniLM-L6-v2
pub struct Embedder {
    tokenizer: Arc<Tokenizer>,
    model: Arc<Mutex<BertModel>>,
    device: Device,
}

impl Embedder {
    /// Initialize the embedder with embedded model files
    pub fn new() -> Result<Self, String> {
        // Initialize device (CPU only for now)
        let device = Device::Cpu;

        // Load tokenizer from embedded JSON
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {e}"))?;

        // Load config from embedded JSON
        let config: Config = serde_json::from_slice(CONFIG_JSON)
            .map_err(|e| format!("Failed to parse model config: {e}"))?;

        // Load model weights from embedded safetensors
        let mut tensors = std::collections::HashMap::new();
        let safetensors = SafeTensors::deserialize(MODEL_SAFETENSORS)
            .map_err(|e| format!("Failed to deserialize model weights: {e}"))?;

        // Load all tensors from safetensors
        for name in safetensors.names() {
            let tensor_view = safetensors.tensor(&name)
                .map_err(|e| format!("Failed to get tensor {}: {e}", name))?;
            let shape = tensor_view.shape().to_vec();

            // Map safetensors dtype to candle dtype
            // Note: candle only supports a subset of dtypes
            let dtype = match tensor_view.dtype() {
                safetensors::Dtype::F32 => candle_core::DType::F32,
                safetensors::Dtype::F16 => candle_core::DType::F16,
                safetensors::Dtype::BF16 => candle_core::DType::BF16,
                safetensors::Dtype::I64 => candle_core::DType::I64,
                safetensors::Dtype::U32 => candle_core::DType::U32,
                safetensors::Dtype::U8 => candle_core::DType::U8,
                safetensors::Dtype::F64 => candle_core::DType::F64,
                // Skip unsupported dtypes (I8, I16, I32, etc.)
                _ => {
                    tracing::debug!("Skipping tensor {} with unsupported dtype {:?}", name, tensor_view.dtype());
                    continue;
                }
            };

            let data = tensor_view.data();
            let tensor = Tensor::from_raw_buffer(data, dtype, &shape, &device)
                .map_err(|e| format!("Failed to create tensor {}: {e}", name))?;
            tensors.insert(name.to_string(), tensor);
        }

        let vb = VarBuilder::from_tensors(tensors, candle_core::DType::F32, &device);

        // Load BERT model
        let model = BertModel::load(vb, &config)
            .map_err(|e| format!("Failed to load BERT model: {e}"))?;

        Ok(Self {
            tokenizer: Arc::new(tokenizer),
            model: Arc::new(Mutex::new(model)),
            device,
        })
    }

    /// Generate embeddings for a batch of texts
    /// Returns a Vec of embeddings, each of dimension EMBEDDING_DIM
    pub fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for text in texts {
            let embedding = self.embed(text)?;
            all_embeddings.push(embedding);
        }

        Ok(all_embeddings)
    }

    /// Generate embedding for a single text
    /// Returns a vector of dimension EMBEDDING_DIM (384)
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        // Tokenize input
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| format!("Tokenization failed: {e}"))?;

        // Get token IDs and truncate to max sequence length
        let mut token_ids = encoding.get_ids().to_vec();
        if token_ids.len() > MAX_SEQ_LENGTH {
            token_ids.truncate(MAX_SEQ_LENGTH);
        }

        // Convert to tensor
        let token_ids_len = token_ids.len();
        let token_ids_tensor = Tensor::new(token_ids, &self.device)
            .map_err(|e| format!("Failed to create token tensor: {e}"))?
            .unsqueeze(0)
            .map_err(|e| format!("Failed to unsqueeze: {e}"))?;

        // Create attention mask (all ones for valid tokens)
        let attention_mask = Tensor::ones((1, token_ids_len), candle_core::DType::U32, &self.device)
            .map_err(|e| format!("Failed to create attention mask: {e}"))?;

        // Forward pass through model
        let model = self.model.lock().unwrap();
        let embeddings = model
            .forward(&token_ids_tensor, &attention_mask, None)
            .map_err(|e| format!("Model forward pass failed: {e}"))?;

        // Mean pooling over sequence dimension
        let pooled = embeddings
            .mean(1)
            .map_err(|e| format!("Mean pooling failed: {e}"))?;

        // Normalize the embedding
        let normalized = self.normalize(&pooled)?;

        // Convert to Vec<f32>
        let embedding_vec = normalized
            .squeeze(0)
            .map_err(|e| format!("Failed to squeeze: {e}"))?
            .to_vec1::<f32>()
            .map_err(|e| format!("Failed to convert to vec: {e}"))?;

        // Verify dimension
        if embedding_vec.len() != EMBEDDING_DIM {
            return Err(format!(
                "Unexpected embedding dimension: got {}, expected {}",
                embedding_vec.len(),
                EMBEDDING_DIM
            ));
        }

        Ok(embedding_vec)
    }

    /// L2 normalize a tensor
    fn normalize(&self, tensor: &Tensor) -> Result<Tensor, String> {
        let norm = tensor
            .sqr()
            .map_err(|e| format!("Failed to square: {e}"))?
            .sum_keepdim(1)
            .map_err(|e| format!("Failed to sum: {e}"))?
            .sqrt()
            .map_err(|e| format!("Failed to sqrt: {e}"))?;

        tensor
            .broadcast_div(&norm)
            .map_err(|e| format!("Failed to normalize: {e}"))
    }
}