//! Search functionality for SDK docs and components.
//!
//! Uses SQLite FTS5 for all search operations.

pub mod common;
pub mod component_index;
pub mod docs_index;

pub use component_index::ComponentIndex;

// Re-export for external use
#[allow(unused_imports)]
pub use docs_index::{DocSearchResult, SDKDocsIndex};
