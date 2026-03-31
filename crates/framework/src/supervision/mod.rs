//! Process lifecycle: supervisor, workers, IPC, shutdown.

pub mod dev_watcher;
pub mod ipc;
pub mod signal;
pub mod supervisor;
pub mod worker;
pub mod worker_context;
