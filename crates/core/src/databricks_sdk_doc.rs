//! Databricks SDK documentation retrieval and parsing.
//!
//! This module re-exports from `sources::databricks_sdk` for backward compatibility.

pub use crate::sources::databricks_sdk::{SDKSource, download_and_extract_sdk, load_doc_files};
