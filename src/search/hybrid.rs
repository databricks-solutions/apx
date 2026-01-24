//! Reusable hybrid search infrastructure for combining vector and FTS search
//!
//! This module provides common utilities for implementing hybrid search that combines:
//! - Vector (semantic) search via embeddings
//! - Full-Text Search (FTS) for exact keyword matches
//! - Reciprocal Rank Fusion (RRF) for merging results

use std::collections::HashMap;
use arrow::array::{Array, StringArray};
use arrow::record_batch::RecordBatch;

/// Configuration for hybrid search behavior
#[derive(Debug, Clone)]
pub struct HybridSearchConfig {
    /// RRF constant (default: 60.0)
    pub rrf_k: f32,
    /// Candidate pool size = limit * multiplier (default: 10)
    pub candidate_pool_multiplier: usize,
    /// Weight for vector search results (default: 1.0)
    pub vector_weight: f32,
    /// Weight for FTS search results (default: 1.0)
    pub fts_weight: f32,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            candidate_pool_multiplier: 10,
            vector_weight: 1.0,
            fts_weight: 1.0,
        }
    }
}

#[allow(dead_code)]
impl HybridSearchConfig {
    /// Create a new config with default values
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Set RRF constant
    pub fn with_rrf_k(mut self, k: f32) -> Self {
        self.rrf_k = k;
        self
    }
    
    /// Set candidate pool multiplier
    pub fn with_pool_multiplier(mut self, multiplier: usize) -> Self {
        self.candidate_pool_multiplier = multiplier;
        self
    }
}

/// Generic search candidate for RRF fusion
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SearchCandidate {
    pub id: String,
    pub vector_rank: Option<usize>,
    pub fts_rank: Option<usize>,
    pub metadata: HashMap<String, String>,
}

#[allow(dead_code)]
impl SearchCandidate {
    /// Create a new candidate with just ID
    pub fn new(id: String) -> Self {
        Self {
            id,
            vector_rank: None,
            fts_rank: None,
            metadata: HashMap::new(),
        }
    }
    
    /// Set vector rank
    pub fn with_vector_rank(mut self, rank: usize) -> Self {
        self.vector_rank = Some(rank);
        self
    }
    
    /// Set FTS rank
    pub fn with_fts_rank(mut self, rank: usize) -> Self {
        self.fts_rank = Some(rank);
        self
    }
    
    /// Add metadata field
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
}

/// Compute Reciprocal Rank Fusion (RRF) score
/// 
/// RRF formula: score = sum(weight / (k + rank)) for each ranking list where item appears
/// 
/// # Arguments
/// * `vector_rank` - Rank in vector search results (0-based)
/// * `fts_rank` - Rank in FTS search results (0-based)
/// * `config` - Hybrid search configuration
/// 
/// # Returns
/// Combined RRF score (higher is better)
pub fn compute_rrf_score(
    vector_rank: Option<usize>,
    fts_rank: Option<usize>,
    config: &HybridSearchConfig,
) -> f32 {
    let mut score = 0.0;
    
    if let Some(rank) = vector_rank {
        score += config.vector_weight / (config.rrf_k + rank as f32);
    }
    
    if let Some(rank) = fts_rank {
        score += config.fts_weight / (config.rrf_k + rank as f32);
    }
    
    score
}

/// Extract a string column from an Arrow RecordBatch
/// 
/// # Arguments
/// * `batch` - The record batch to extract from
/// * `column_name` - Name of the column to extract
/// 
/// # Returns
/// Vector of strings from the column
#[allow(dead_code)]
pub fn extract_string_column(
    batch: &RecordBatch,
    column_name: &str,
) -> Result<Vec<String>, String> {
    let array = batch
        .column_by_name(column_name)
        .ok_or_else(|| format!("Missing {} column", column_name))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| format!("Failed to downcast {} column", column_name))?;
    
    Ok((0..array.len())
        .map(|i| array.value(i).to_string())
        .collect())
}

/// Extract multiple string columns from an Arrow RecordBatch
/// 
/// # Arguments
/// * `batch` - The record batch to extract from
/// * `column_names` - Names of the columns to extract
/// 
/// # Returns
/// Vector of tuples, one per row, with values from each column
#[allow(dead_code)]
pub fn extract_string_columns(
    batch: &RecordBatch,
    column_names: &[&str],
) -> Result<Vec<Vec<String>>, String> {
    let mut columns = Vec::new();
    
    for &col_name in column_names {
        columns.push(extract_string_column(batch, col_name)?);
    }
    
    // Transpose: from Vec<Vec<String>> (column-major) to Vec<Vec<String>> (row-major)
    let num_rows = batch.num_rows();
    let mut rows = Vec::with_capacity(num_rows);
    
    for i in 0..num_rows {
        let mut row = Vec::with_capacity(columns.len());
        for column in &columns {
            row.push(column[i].clone());
        }
        rows.push(row);
    }
    
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rrf_score_both_ranks() {
        let config = HybridSearchConfig::default();
        let score = compute_rrf_score(Some(0), Some(0), &config);
        // 1/(60+0) + 1/(60+0) = 2/60 ≈ 0.0333
        assert!((score - 0.0333).abs() < 0.001);
    }
    
    #[test]
    fn test_rrf_score_vector_only() {
        let config = HybridSearchConfig::default();
        let score = compute_rrf_score(Some(0), None, &config);
        // 1/(60+0) ≈ 0.0167
        assert!((score - 0.0167).abs() < 0.001);
    }
    
    #[test]
    fn test_rrf_score_fts_only() {
        let config = HybridSearchConfig::default();
        let score = compute_rrf_score(None, Some(0), &config);
        // 1/(60+0) ≈ 0.0167
        assert!((score - 0.0167).abs() < 0.001);
    }
    
    #[test]
    fn test_rrf_score_with_weights() {
        let config = HybridSearchConfig {
            rrf_k: 60.0,
            vector_weight: 2.0,
            fts_weight: 1.0,
            candidate_pool_multiplier: 10,
        };
        let score = compute_rrf_score(Some(0), Some(0), &config);
        // 2/(60+0) + 1/(60+0) = 3/60 = 0.05
        assert!((score - 0.05).abs() < 0.001);
    }
}
