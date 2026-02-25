use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use apx_core::components::SharedCacheState;
use apx_core::search::docs_index::SDKDocsIndex;
use apx_db::DevDb;
use tokio::sync::{Mutex, Notify, RwLock, broadcast};

/// Parameters for SDK indexing, pre-computed synchronously to avoid Python GIL issues.
#[derive(Debug)]
pub struct SdkIndexParams {
    /// Detected Databricks SDK version string.
    pub sdk_version: String,
    /// Shared handle to the SDK docs index (populated after bootstrap).
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
}

/// State for tracking index readiness
#[derive(Clone, Debug)]
pub struct IndexState {
    /// Notifies waiters when component index is ready
    pub component_ready: Arc<Notify>,
    /// Notifies waiters when SDK docs index is ready
    pub sdk_ready: Arc<Notify>,
    /// Whether component indexing has completed (for late subscribers)
    pub component_indexed: Arc<AtomicBool>,
    /// Whether SDK indexing has completed (for late subscribers)
    pub sdk_indexed: Arc<AtomicBool>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            component_ready: Arc::new(Notify::new()),
            sdk_ready: Arc::new(Notify::new()),
            component_indexed: Arc::new(AtomicBool::new(false)),
            sdk_indexed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl IndexState {
    /// Create a new `IndexState` with all indexes marked as not ready.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Global application context shared across all MCP handlers.
#[derive(Debug)]
pub struct AppContext {
    /// Development database handle.
    pub dev_db: DevDb,
    /// Shared SDK documentation search index.
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
    /// Shared component cache state.
    pub cache_state: SharedCacheState,
    /// Readiness state for background indexes.
    pub index_state: IndexState,
    /// Broadcast channel to signal server shutdown.
    pub shutdown_tx: broadcast::Sender<()>,
    /// Cached Databricks API clients keyed by profile name.
    pub databricks_clients: RwLock<HashMap<String, apx_databricks_sdk::DatabricksClient>>,
}
