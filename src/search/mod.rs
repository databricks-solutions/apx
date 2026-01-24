/// Search functionality for SDK docs and components.
///
/// - SDK docs: Hybrid vector + FTS search using embedded models
/// - Components: Hybrid vector + FTS search using embedded models
pub mod embedded_model;
pub mod embedder;
pub mod component_index;
pub mod common;
pub mod hybrid;
pub mod docs_index;

pub use component_index::ComponentIndex;

// Re-export for external use
#[allow(unused_imports)]
pub use docs_index::{SDKDocsIndex, DocSearchResult};
