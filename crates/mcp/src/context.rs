use apx_core::components::SharedCacheState;
use apx_core::search::docs_index::SDKDocsIndex;
use apx_db::DevDb;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, Notify, broadcast};

/// Parameters for SDK indexing, pre-computed synchronously to avoid Python GIL issues
#[derive(Debug)]
pub struct SdkIndexParams {
    pub sdk_version: String,
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
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug)]
pub struct AppContext {
    pub dev_db: DevDb,
    pub sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
    pub cache_state: SharedCacheState,
    pub index_state: IndexState,
    pub shutdown_tx: broadcast::Sender<()>,
}
