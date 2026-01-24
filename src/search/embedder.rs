use ort::session::Session;
use ort::value::Tensor;
use ndarray::{Array2, Axis};
use tokenizers::Tokenizer;
use std::sync::{Arc, Mutex};

use super::embedded_model::{
    MODEL_ONNX, TOKENIZER_JSON, EMBEDDING_DIM, MAX_SEQ_LENGTH,
};

/// Thread-safe embedding generator using bge-small-en-v1.5
pub struct Embedder {
    tokenizer: Arc<Tokenizer>,
    session: Arc<Mutex<Session>>,
}

impl Embedder {
    /// Initialize the embedder with embedded model files
    pub fn new() -> Result<Self, String> {
        // Load tokenizer from embedded JSON
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {e}"))?;

        // Load ONNX model from embedded bytes
        let session = Session::builder()
            .map_err(|e| format!("Failed to create session builder: {e}"))?
            .commit_from_memory(MODEL_ONNX)
            .map_err(|e| format!("Failed to load ONNX model: {e}"))?;

        Ok(Self {
            tokenizer: Arc::new(tokenizer),
            session: Arc::new(Mutex::new(session)),
        })
    }

    /// Generate embeddings for a batch of texts using true batching
    /// Returns a Vec of embeddings, each of dimension EMBEDDING_DIM
    pub fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let start = std::time::Instant::now();
        let total_texts = texts.len();
        
        // Process in smaller batches for efficiency (32 texts per batch)
        const BATCH_SIZE: usize = 32;
        
        if total_texts > BATCH_SIZE {
            tracing::info!("embed_batch: Processing {} texts in batches of {}", total_texts, BATCH_SIZE);
            let mut all_embeddings = Vec::with_capacity(total_texts);
            
            for (i, chunk) in texts.chunks(BATCH_SIZE).enumerate() {
                let chunk_start = std::time::Instant::now();
                let chunk_embeddings = self.embed_batch_internal(chunk)?;
                all_embeddings.extend(chunk_embeddings);
                tracing::debug!("embed_batch: Batch {}/{} ({} texts) took {:?}", 
                    i + 1, (total_texts + BATCH_SIZE - 1) / BATCH_SIZE, chunk.len(), chunk_start.elapsed());
            }
            
            tracing::info!("embed_batch: Total time for {} texts: {:?}", total_texts, start.elapsed());
            return Ok(all_embeddings);
        }

        self.embed_batch_internal(texts)
    }

    /// Internal batch processing for a small number of texts
    fn embed_batch_internal(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let batch_size = texts.len();
        tracing::debug!("embed_batch_internal: Processing {} texts", batch_size);

        // Tokenize all texts
        let tokenize_start = std::time::Instant::now();
        let encodings: Vec<_> = texts.iter()
            .map(|text| self.tokenizer.encode(text.as_str(), true))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Tokenization failed: {e}"))?;
        tracing::debug!("embed_batch: Tokenization took {:?}", tokenize_start.elapsed());

        // Find max sequence length for padding (capped at MAX_SEQ_LENGTH)
        let max_len = encodings.iter()
            .map(|e| e.get_ids().len().min(MAX_SEQ_LENGTH))
            .max()
            .unwrap_or(0);

        // Prepare padded batched inputs
        let prep_start = std::time::Instant::now();
        let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut token_type_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in &encodings {
            let mut ids = encoding.get_ids().to_vec();
            if ids.len() > MAX_SEQ_LENGTH {
                ids.truncate(MAX_SEQ_LENGTH);
            }
            let actual_len = ids.len();

            // Add input_ids with padding
            input_ids_flat.extend(ids.iter().map(|&x| x as i64));
            input_ids_flat.extend(std::iter::repeat(0i64).take(max_len - actual_len));

            // Add attention_mask (1 for real tokens, 0 for padding)
            attention_mask_flat.extend(std::iter::repeat(1i64).take(actual_len));
            attention_mask_flat.extend(std::iter::repeat(0i64).take(max_len - actual_len));

            // Add token_type_ids (all zeros)
            token_type_ids_flat.extend(std::iter::repeat(0i64).take(max_len));
        }
        tracing::debug!("embed_batch: Input preparation took {:?}", prep_start.elapsed());

        // Create batched ndarray inputs with shape [batch_size, max_len]
        let tensor_start = std::time::Instant::now();
        let input_ids_array = Array2::from_shape_vec((batch_size, max_len), input_ids_flat)
            .map_err(|e| format!("Failed to create input_ids array: {e}"))?;

        let attention_mask_array = Array2::from_shape_vec((batch_size, max_len), attention_mask_flat)
            .map_err(|e| format!("Failed to create attention_mask array: {e}"))?;

        let token_type_ids_array = Array2::from_shape_vec((batch_size, max_len), token_type_ids_flat)
            .map_err(|e| format!("Failed to create token_type_ids array: {e}"))?;

        // Create tensors from arrays
        let input_ids_tensor = Tensor::from_array(input_ids_array)
            .map_err(|e| format!("Failed to create input_ids tensor: {e}"))?;
        
        let attention_mask_tensor = Tensor::from_array(attention_mask_array)
            .map_err(|e| format!("Failed to create attention_mask tensor: {e}"))?;

        let token_type_ids_tensor = Tensor::from_array(token_type_ids_array)
            .map_err(|e| format!("Failed to create token_type_ids tensor: {e}"))?;
        tracing::debug!("embed_batch: Tensor creation took {:?}", tensor_start.elapsed());

        // Run batched inference
        let inference_start = std::time::Instant::now();
        let mut session = self.session.lock().unwrap();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(|e| format!("Model forward pass failed: {e}"))?;
        tracing::debug!("embed_batch: Inference took {:?}", inference_start.elapsed());

        // Extract embeddings
        let extract_start = std::time::Instant::now();
        let embeddings_tensor = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Failed to extract embeddings: {e}"))?;

        let (shape, data) = embeddings_tensor;
        let shape_dims = shape.as_ref();

        // Shape should be [batch_size, seq_len, hidden_dim]
        if shape_dims.len() != 3 || shape_dims[0] as usize != batch_size {
            return Err(format!(
                "Unexpected embedding shape: {:?}, expected [{}, {}, 384]",
                shape_dims, batch_size, max_len
            ));
        }

        let seq_len_output = shape_dims[1] as usize;
        let hidden_dim = shape_dims[2] as usize;

        // Process each sample in the batch
        let mut all_embeddings = Vec::with_capacity(batch_size);
        let stride = seq_len_output * hidden_dim;

        for (i, encoding) in encodings.iter().enumerate() {
            // Get the embeddings for this sample
            let sample_start = i * stride;
            let sample_data = &data[sample_start..sample_start + stride];

            // Get actual sequence length (excluding padding)
            let actual_len = encoding.get_ids().len().min(MAX_SEQ_LENGTH);

            // Reshape to [seq_len, hidden_dim] and take only non-padded tokens
            let embeddings_array = Array2::from_shape_vec((seq_len_output, hidden_dim), sample_data.to_vec())
                .map_err(|e| format!("Failed to reshape embeddings: {e}"))?;

            // Mean pooling over actual tokens only (not padding)
            // Use slice_axis to avoid the s! macro which requires unsafe
            let actual_embeddings = embeddings_array.slice_axis(
                Axis(0),
                ndarray::Slice::from(0..actual_len)
            );
            let pooled = actual_embeddings.mean_axis(Axis(0))
                .ok_or_else(|| "Failed to perform mean pooling".to_string())?;

            // L2 normalize
            let normalized = self.normalize_ndarray(&pooled.view())?;
            let embedding_vec: Vec<f32> = normalized.to_vec();

            if embedding_vec.len() != EMBEDDING_DIM {
                return Err(format!(
                    "Unexpected embedding dimension: got {}, expected {}",
                    embedding_vec.len(),
                    EMBEDDING_DIM
                ));
            }

            all_embeddings.push(embedding_vec);
        }
        tracing::debug!("embed_batch_internal: Post-processing took {:?}", extract_start.elapsed());

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

        let seq_len = token_ids.len();

        // Convert to i64 for ONNX Runtime
        let input_ids: Vec<i64> = token_ids.iter().map(|&x| x as i64).collect();

        // Create attention mask (all ones for valid tokens)
        let attention_mask: Vec<i64> = vec![1i64; seq_len];

        // Create token type ids (all zeros for single sequence)
        let token_type_ids: Vec<i64> = vec![0i64; seq_len];

        // Create ndarray inputs with shape [1, seq_len]
        let input_ids_array = Array2::from_shape_vec((1, seq_len), input_ids)
            .map_err(|e| format!("Failed to create input_ids array: {e}"))?;

        let attention_mask_array = Array2::from_shape_vec((1, seq_len), attention_mask)
            .map_err(|e| format!("Failed to create attention_mask array: {e}"))?;

        let token_type_ids_array = Array2::from_shape_vec((1, seq_len), token_type_ids)
            .map_err(|e| format!("Failed to create token_type_ids array: {e}"))?;

        // Create tensors from arrays
        let input_ids_tensor = Tensor::from_array(input_ids_array)
            .map_err(|e| format!("Failed to create input_ids tensor: {e}"))?;
        
        let attention_mask_tensor = Tensor::from_array(attention_mask_array)
            .map_err(|e| format!("Failed to create attention_mask tensor: {e}"))?;

        let token_type_ids_tensor = Tensor::from_array(token_type_ids_array)
            .map_err(|e| format!("Failed to create token_type_ids tensor: {e}"))?;

        // Run inference
        let mut session = self.session.lock().unwrap();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(|e| format!("Model forward pass failed: {e}"))?;

        // Extract embeddings from last_hidden_state
        // bge models typically use mean pooling over the sequence
        let embeddings_tensor = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Failed to extract embeddings: {e}"))?;

        // embeddings_tensor is (&Shape, &[f32])
        let (shape, data) = embeddings_tensor;
        let shape_dims = shape.as_ref();

        // Shape should be [batch_size, seq_len, hidden_dim] = [1, seq_len, 384]
        if shape_dims.len() != 3 || shape_dims[0] != 1 {
            return Err(format!(
                "Unexpected embedding shape: {:?}, expected [1, seq_len, 384]",
                shape_dims
            ));
        }

        let _batch_size = shape_dims[0];
        let seq_len_output = shape_dims[1] as usize;
        let hidden_dim = shape_dims[2] as usize;

        // Reshape data into ndarray for mean pooling
        let embeddings_array = Array2::from_shape_vec((seq_len_output, hidden_dim), data.to_vec())
            .map_err(|e| format!("Failed to reshape embeddings: {e}"))?;

        // Mean pooling over sequence dimension
        let pooled = embeddings_array.mean_axis(Axis(0))
            .ok_or_else(|| "Failed to perform mean pooling".to_string())?;

        // L2 normalize the embedding
        let normalized = self.normalize_ndarray(&pooled.view())?;

        // Convert to Vec<f32>
        let embedding_vec: Vec<f32> = normalized.to_vec();

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

    /// L2 normalize an ndarray
    fn normalize_ndarray(&self, arr: &ndarray::ArrayView1<f32>) -> Result<ndarray::Array1<f32>, String> {
        // Calculate L2 norm
        let norm = arr.iter()
            .map(|&x| x * x)
            .sum::<f32>()
            .sqrt();

        if norm == 0.0 {
            return Err("Cannot normalize zero vector".to_string());
        }

        // Divide by norm
        let normalized = arr.mapv(|x| x / norm);

        Ok(normalized)
    }
}
