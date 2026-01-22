/// Search functionality for SDK docs and components.
///
/// - SDK docs: Hybrid vector + FTS search using embedded models
/// - Components: FTS-only search (no embeddings needed for short text)

pub mod embedded_model;
pub mod embedder;
pub mod component_index;
pub mod common;

pub use component_index::ComponentIndex;
