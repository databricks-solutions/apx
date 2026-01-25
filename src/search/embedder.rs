use ort::session::Session;
use ort::value::Tensor;
use ndarray::{Array2, Axis};
use rayon::prelude::*;
use tokenizers::{Tokenizer, Encoding};
use std::sync::Mutex;

use crate::common::Timer;
use super::embedded_model::{
    MODEL_ONNX, TOKENIZER_JSON, EMBEDDING_DIM, MAX_SEQ_LENGTH,
};

/// Batch size for ONNX inference (larger = fewer batches = less overhead)
const INFERENCE_BATCH_SIZE: usize = 128;

/// Thread-safe embedding generator using bge-small-en-v1.5
/// Architecture: tokenize ALL texts first, then batch ONNX inference
pub struct Embedder {
    tokenizer: Tokenizer,
    session: Mutex<Session>,
}

impl Embedder {
    /// Initialize the embedder with embedded model files
    pub fn new() -> Result<Self, String> {
        // Load tokenizer from embedded JSON
        let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
            .map_err(|e| format!("Failed to load tokenizer: {e}"))?;

        // Create single ONNX session with full CPU utilization
        let num_cpus = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);
        
        tracing::info!(
            "Creating ONNX session with {} intra-op threads",
            num_cpus
        );

        let session = Session::builder()
            .map_err(|e| format!("Failed to create session builder: {e}"))?
            .with_intra_threads(num_cpus)
            .map_err(|e| format!("Failed to set intra threads: {e}"))?
            .commit_from_memory(MODEL_ONNX)
            .map_err(|e| format!("Failed to load ONNX model: {e}"))?;

        Ok(Self {
            tokenizer,
            session: Mutex::new(session),
        })
    }

    /// Generate embeddings for a batch of texts using optimized pipeline:
    /// 1. Tokenize ALL texts upfront (single pass, no parallelism contention)
    /// 2. Batch encodings for ONNX inference
    /// 3. Run inference with full CPU utilization per batch
    pub fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let total_texts = texts.len();
        let batch_timer = Timer::start(format!("embed_batch[{} texts]", total_texts));
        
        // Step 1: Tokenize ALL texts upfront (this is the key optimization)
        let tokenize_timer = Timer::start("tokenize_all");
        let encodings = self.tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| format!("Batch tokenization failed: {e}"))?;
        tokenize_timer.lap(&format!("Tokenized all {} texts", total_texts));
        
        // Step 2: Process in batches for ONNX inference
        let num_batches = (total_texts + INFERENCE_BATCH_SIZE - 1) / INFERENCE_BATCH_SIZE;
        tracing::info!(
            "embed_batch: Processing {} texts in {} batches of {} (tokenization done)",
            total_texts, num_batches, INFERENCE_BATCH_SIZE
        );
        
        let mut all_embeddings = Vec::with_capacity(total_texts);
        
        // Acquire session lock once for all batches (avoid lock contention)
        let mut session = self.session.lock().unwrap();
        
        for (batch_idx, encoding_batch) in encodings.chunks(INFERENCE_BATCH_SIZE).enumerate() {
            let inference_timer = Timer::start(format!("inference_batch_{}", batch_idx + 1));
            
            let batch_embeddings = self.run_inference_on_encodings(&mut session, encoding_batch)?;
            
            inference_timer.lap(&format!(
                "Batch {}/{} ({} texts)",
                batch_idx + 1, num_batches, encoding_batch.len()
            ));
            
            all_embeddings.extend(batch_embeddings);
        }
        
        batch_timer.lap(&format!("Generated {} embeddings", all_embeddings.len()));
        Ok(all_embeddings)
    }
    
    /// Run ONNX inference on pre-tokenized encodings
    fn run_inference_on_encodings(
        &self,
        session: &mut Session,
        encodings: &[Encoding],
    ) -> Result<Vec<Vec<f32>>, String> {
        let batch_size = encodings.len();
        
        // Find max sequence length for padding (capped at MAX_SEQ_LENGTH)
        let max_len = encodings.iter()
            .map(|e| e.get_ids().len().min(MAX_SEQ_LENGTH))
            .max()
            .unwrap_or(0);

        // Prepare padded batched inputs
        let prep_timer = Timer::start("input_preparation");
        let mut input_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut token_type_ids_flat: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in encodings {
            let ids = encoding.get_ids();
            let actual_len = ids.len().min(MAX_SEQ_LENGTH);

            // Add input_ids with padding
            input_ids_flat.extend(ids.iter().take(actual_len).map(|&x| x as i64));
            input_ids_flat.extend(std::iter::repeat(0i64).take(max_len - actual_len));

            // Add attention_mask (1 for real tokens, 0 for padding)
            attention_mask_flat.extend(std::iter::repeat(1i64).take(actual_len));
            attention_mask_flat.extend(std::iter::repeat(0i64).take(max_len - actual_len));

            // Add token_type_ids (all zeros)
            token_type_ids_flat.extend(std::iter::repeat(0i64).take(max_len));
        }
        prep_timer.lap(&format!("Prepared {} inputs", batch_size));

        // Create batched ndarray inputs
        let tensor_timer = Timer::start("tensor_creation");
        let input_ids_array = Array2::from_shape_vec((batch_size, max_len), input_ids_flat)
            .map_err(|e| format!("Failed to create input_ids array: {e}"))?;
        let attention_mask_array = Array2::from_shape_vec((batch_size, max_len), attention_mask_flat)
            .map_err(|e| format!("Failed to create attention_mask array: {e}"))?;
        let token_type_ids_array = Array2::from_shape_vec((batch_size, max_len), token_type_ids_flat)
            .map_err(|e| format!("Failed to create token_type_ids array: {e}"))?;

        let input_ids_tensor = Tensor::from_array(input_ids_array)
            .map_err(|e| format!("Failed to create input_ids tensor: {e}"))?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_array)
            .map_err(|e| format!("Failed to create attention_mask tensor: {e}"))?;
        let token_type_ids_tensor = Tensor::from_array(token_type_ids_array)
            .map_err(|e| format!("Failed to create token_type_ids tensor: {e}"))?;
        tensor_timer.finish();

        // Run ONNX inference
        let inference_timer = Timer::start("onnx_inference");
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(|e| format!("Model forward pass failed: {e}"))?;
        inference_timer.finish();

        // Extract and post-process embeddings
        let postprocess_timer = Timer::start("post_processing");
        let embeddings_tensor = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("Failed to extract embeddings: {e}"))?;

        let (shape, data) = embeddings_tensor;
        let shape_dims = shape.as_ref();

        if shape_dims.len() != 3 || shape_dims[0] as usize != batch_size {
            return Err(format!(
                "Unexpected embedding shape: {:?}, expected [{}, {}, 384]",
                shape_dims, batch_size, max_len
            ));
        }

        let seq_len_output = shape_dims[1] as usize;
        let hidden_dim = shape_dims[2] as usize;
        let stride = seq_len_output * hidden_dim;

        // Process each sample (parallel post-processing is fine - it's cheap)
        let all_embeddings: Vec<Vec<f32>> = encodings.par_iter()
            .enumerate()
            .map(|(i, encoding)| {
                let sample_start = i * stride;
                let sample_data = &data[sample_start..sample_start + stride];
                let actual_len = encoding.get_ids().len().min(MAX_SEQ_LENGTH);

                let embeddings_array = Array2::from_shape_vec((seq_len_output, hidden_dim), sample_data.to_vec())
                    .map_err(|e| format!("Failed to reshape embeddings: {e}"))?;

                let actual_embeddings = embeddings_array.slice_axis(
                    Axis(0),
                    ndarray::Slice::from(0..actual_len)
                );
                let pooled = actual_embeddings.mean_axis(Axis(0))
                    .ok_or_else(|| "Failed to perform mean pooling".to_string())?;

                let normalized = self.normalize_ndarray(&pooled.view())?;
                let embedding_vec: Vec<f32> = normalized.to_vec();

                if embedding_vec.len() != EMBEDDING_DIM {
                    return Err(format!(
                        "Unexpected embedding dimension: got {}, expected {}",
                        embedding_vec.len(), EMBEDDING_DIM
                    ));
                }

                Ok(embedding_vec)
            })
            .collect::<Result<Vec<_>, String>>()?;
        
        postprocess_timer.lap(&format!("Post-processed {} embeddings", batch_size));
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
