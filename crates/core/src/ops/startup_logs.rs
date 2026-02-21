//! Log streaming for dev server startup.
//!
//! Prints real-time logs line-by-line during server startup.

use std::path::Path;

use apx_common::format::format_startup_log;
use apx_common::should_skip_log;
use apx_db::LogsDb;

use crate::common::{OutputMode, emit};

/// Simple log streamer that prints logs line-by-line to stdout.
pub struct StartupLogStreamer {
    last_log_id: i64,
    storage: Option<LogsDb>,
    app_path: String,
    mode: OutputMode,
}

impl std::fmt::Debug for StartupLogStreamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupLogStreamer")
            .field("last_log_id", &self.last_log_id)
            .field("storage", &self.storage.as_ref().map(|_| ".."))
            .field("app_path", &self.app_path)
            .finish()
    }
}

impl StartupLogStreamer {
    /// Create a new log streamer for the given app directory.
    pub async fn new(app_dir: &Path, mode: OutputMode) -> Self {
        let app_path = app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf())
            .display()
            .to_string();

        let storage = LogsDb::open().await.ok();
        let last_log_id = match &storage {
            Some(s) => s.get_latest_id().await.unwrap_or(0),
            None => 0,
        };

        Self {
            last_log_id,
            storage,
            app_path,
            mode,
        }
    }

    /// Print any new logs since the last call.
    /// Returns the number of new log lines printed.
    pub async fn print_new_logs(&mut self) -> usize {
        let storage = match &self.storage {
            Some(s) => s,
            None => return 0,
        };

        // Query logs since last ID
        let records = match storage
            .query_logs_after_id(Some(&self.app_path), self.last_log_id)
            .await
        {
            Ok(r) => r,
            Err(_) => return 0,
        };

        let mut count = 0;
        for record in &records {
            if !should_skip_log(record) {
                emit(self.mode, &format_startup_log(record));
                count += 1;
            }
        }

        // Update last_log_id
        if let Ok(new_id) = storage.get_latest_id().await
            && new_id > self.last_log_id
        {
            self.last_log_id = new_id;
        }

        count
    }
}

// format_startup_log and format_short_timestamp are provided by apx_common::format.
