/// Search functionality for SDK docs and components.
///
/// - SDK docs: Hybrid vector + FTS search using embedded models
/// - Components: Hybrid vector + FTS search using embedded models

pub mod embedded_model;
pub mod embedder;
pub mod component_index;
pub mod common;
pub mod hybrid;

pub use component_index::ComponentIndex;
