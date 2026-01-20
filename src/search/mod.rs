/// Semantic search functionality using embedded models.
///
/// This module provides tokenization and embedding generation for semantic search.
/// Models are embedded at compile time for zero-dependency runtime.

pub mod embedded_model;
pub mod embedder;
pub mod component_index;
pub mod common;

pub use component_index::ComponentIndex;
