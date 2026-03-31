//! File watcher for dev-mode hot reload.
//!
//! Watches a directory recursively for Python source file changes and
//! emits reload signals. The supervisor uses these signals to restart
//! worker processes so they re-import updated code.
//!
//! # Design
//!
//! Event classification and path filtering are pure functions (sans-I/O),
//! testable without `notify` infrastructure. The `DevWatcher` struct wraps
//! the `notify` watcher and exposes an async `recv()` API.

use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio::time::Duration;

/// Debounce window for rapid file changes.
const DEBOUNCE_MS: u64 = 150;

/// File extension that triggers a reload.
const WATCHED_EXTENSION: &str = "py";

/// Directory name to ignore (Python bytecode cache).
const PYCACHE_DIR: &str = "__pycache__";

/// File extension to ignore (compiled bytecode).
const BYTECODE_EXTENSION: &str = "pyc";

/// Reload signal carrying the list of changed files.
#[derive(Debug, Clone)]
pub struct ReloadInfo {
    /// Python source files that changed, deduplicated.
    pub files: Vec<PathBuf>,
}

/// Errors from creating the dev file watcher.
#[derive(Debug, thiserror::Error)]
pub enum DevWatcherError {
    /// `notify` watcher creation or registration failed.
    #[error("file watcher failed: {0}")]
    Notify(#[from] notify::Error),
}

/// Watches a directory for Python source changes and signals reload.
///
/// Create with [`DevWatcher::new`], then call [`recv`](DevWatcher::recv) in
/// a loop to await reload signals. Each signal means "at least one `.py`
/// file changed since the last signal."
pub struct DevWatcher {
    watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<ReloadInfo>,
}

impl std::fmt::Debug for DevWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevWatcher").finish_non_exhaustive()
    }
}

impl DevWatcher {
    /// Start watching `watch_dir` recursively for `.py` file changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be created or the directory
    /// cannot be registered.
    pub fn new(watch_dir: &Path) -> Result<Self, DevWatcherError> {
        let (event_tx, event_rx) = mpsc::channel::<Event>(256);

        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = event_tx.blocking_send(event);
            }
        })?;

        let (reload_tx, reload_rx) = mpsc::channel::<ReloadInfo>(1);
        let dir = watch_dir.to_path_buf();

        tokio::spawn(debounce_loop(event_rx, reload_tx));

        let mut dw = Self {
            watcher,
            rx: reload_rx,
        };
        dw.register(&dir)?;
        Ok(dw)
    }

    /// Wait for the next reload signal.
    ///
    /// Returns `Some(ReloadInfo)` when at least one `.py` file changed,
    /// or `None` if the watcher was dropped.
    pub async fn recv(&mut self) -> Option<ReloadInfo> {
        self.rx.recv().await
    }

    /// Register the watch directory with the underlying watcher.
    fn register(&mut self, dir: &Path) -> Result<(), DevWatcherError> {
        self.watcher.watch(dir, RecursiveMode::Recursive)?;
        Ok(())
    }
}

// ── Pure classification functions ────────────────────────────────────────

/// Returns `true` if the event kind is a file create or modify.
fn is_relevant_kind(kind: EventKind) -> bool {
    matches!(kind, EventKind::Modify(_) | EventKind::Create(_))
}

/// Returns `true` if `path` is a Python source file worth reloading for.
///
/// Accepts `.py` files that are not inside `__pycache__` directories.
/// Rejects `.pyc` bytecode files.
pub fn should_watch(path: &Path) -> bool {
    if has_pycache_ancestor(path) {
        return false;
    }
    matches_extension(path, WATCHED_EXTENSION) && !matches_extension(path, BYTECODE_EXTENSION)
}

/// Returns `true` if any ancestor directory is named `__pycache__`.
fn has_pycache_ancestor(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, std::path::Component::Normal(s) if s == PYCACHE_DIR))
}

/// Returns `true` if the path's extension matches `ext`.
fn matches_extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e == ext)
}

/// Returns `true` if `event` represents a Python source file change.
pub fn is_reload_event(event: &Event) -> bool {
    is_relevant_kind(event.kind) && event.paths.iter().any(|p| should_watch(p))
}

// ── Debounce loop ────────────────────────────────────────────────────────

/// Background task: receives raw `notify` events, filters and debounces
/// them, then sends a [`ReloadInfo`] with changed file paths per batch.
async fn debounce_loop(mut event_rx: mpsc::Receiver<Event>, reload_tx: mpsc::Sender<ReloadInfo>) {
    while let Some(event) = event_rx.recv().await {
        if !is_reload_event(&event) {
            continue;
        }

        let mut files: Vec<PathBuf> = collect_py_paths(&event);

        tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;
        drain_pending(&mut event_rx, &mut files);

        files.sort();
        files.dedup();

        if reload_tx.send(ReloadInfo { files }).await.is_err() {
            break;
        }
    }
}

/// Extract `.py` file paths from a notify event.
fn collect_py_paths(event: &Event) -> Vec<PathBuf> {
    event
        .paths
        .iter()
        .filter(|p| should_watch(p))
        .cloned()
        .collect()
}

/// Drain all pending events from the channel, collecting `.py` paths.
fn drain_pending(rx: &mut mpsc::Receiver<Event>, files: &mut Vec<PathBuf>) {
    while let Ok(event) = rx.try_recv() {
        if is_reload_event(&event) {
            files.extend(collect_py_paths(&event));
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};
    use std::path::PathBuf;

    fn py_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/project/src/{name}"))
    }

    fn pycache_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/project/src/__pycache__/{name}"))
    }

    fn event_with(kind: EventKind, paths: Vec<PathBuf>) -> Event {
        Event {
            kind,
            paths,
            attrs: notify::event::EventAttributes::default(),
        }
    }

    // ── should_watch ─────────────────────────────────────────────────

    #[test]
    fn should_watch_accepts_py_files() {
        assert!(should_watch(Path::new("app.py")));
        assert!(should_watch(Path::new("/project/src/routes/api.py")));
    }

    #[test]
    fn should_watch_rejects_non_py_files() {
        assert!(!should_watch(Path::new("config.toml")));
        assert!(!should_watch(Path::new("readme.md")));
        assert!(!should_watch(Path::new("data.json")));
        assert!(!should_watch(Path::new("script.sh")));
    }

    #[test]
    fn should_watch_rejects_pyc_files() {
        assert!(!should_watch(Path::new("module.pyc")));
    }

    #[test]
    fn should_watch_rejects_pycache_py_files() {
        assert!(!should_watch(Path::new(
            "/project/__pycache__/module.cpython-312.py"
        )));
        assert!(!should_watch(Path::new("__pycache__/app.py")));
    }

    #[test]
    fn should_watch_rejects_nested_pycache() {
        assert!(!should_watch(Path::new(
            "/project/src/__pycache__/deep/module.py"
        )));
    }

    // ── is_reload_event ──────────────────────────────────────────────

    #[test]
    fn classify_modify_py_file_is_reload() {
        let event = event_with(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            vec![py_path("app.py")],
        );
        assert!(is_reload_event(&event));
    }

    #[test]
    fn classify_create_py_file_is_reload() {
        let event = event_with(
            EventKind::Create(CreateKind::File),
            vec![py_path("new_module.py")],
        );
        assert!(is_reload_event(&event));
    }

    #[test]
    fn classify_modify_non_py_is_not_reload() {
        let event = event_with(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            vec![py_path("config.toml")],
        );
        assert!(!is_reload_event(&event));
    }

    #[test]
    fn classify_remove_py_is_not_reload() {
        let event = event_with(EventKind::Remove(RemoveKind::File), vec![py_path("old.py")]);
        assert!(!is_reload_event(&event));
    }

    #[test]
    fn classify_modify_pycache_is_not_reload() {
        let event = event_with(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            vec![pycache_path("module.cpython-312.py")],
        );
        assert!(!is_reload_event(&event));
    }

    // ── DevWatcher integration ───────────────────────────────────────

    #[tokio::test]
    async fn watcher_detects_py_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = DevWatcher::new(dir.path()).unwrap();

        // Small delay so the OS watcher is fully registered.
        tokio::time::sleep(Duration::from_millis(50)).await;

        std::fs::write(dir.path().join("app.py"), "# changed").unwrap();

        let result = tokio::time::timeout(Duration::from_secs(3), watcher.recv()).await;
        assert!(
            result.is_ok(),
            "expected reload signal after .py file change"
        );
        let info = result.unwrap().unwrap();
        assert!(
            !info.files.is_empty(),
            "expected at least one changed file path"
        );
    }

    #[tokio::test]
    async fn watcher_ignores_non_py() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = DevWatcher::new(dir.path()).unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("config.toml"), "[app]").unwrap();

        let result = tokio::time::timeout(Duration::from_millis(500), watcher.recv()).await;
        assert!(result.is_err(), "expected timeout, no reload for non-py");
    }

    #[tokio::test]
    async fn watcher_debounces_rapid_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = DevWatcher::new(dir.path()).unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        for i in 0..5 {
            std::fs::write(dir.path().join(format!("mod_{i}.py")), format!("# {i}")).unwrap();
        }

        let first = tokio::time::timeout(Duration::from_secs(3), watcher.recv()).await;
        assert!(first.is_ok(), "expected one reload signal");

        let second = tokio::time::timeout(Duration::from_millis(500), watcher.recv()).await;
        assert!(
            second.is_err(),
            "expected no second signal (debounce should coalesce)"
        );
    }
}
