//! Generic polling watcher with graceful shutdown.
//!
//! Provides a [`PollingWatcher`] trait and [`spawn_polling_watcher`] function
//! that handle the common `tokio::spawn` + `loop` + `select!` boilerplate.
//! Each watcher only needs to define what to check and how often.

use std::future::Future;
use std::ops::ControlFlow;

use tokio::sync::broadcast;
use tokio::time::Duration;
use tracing::debug;

use crate::dev::common::Shutdown;

/// A polling watcher that runs in a background task with graceful shutdown.
///
/// Implementors define what to watch and how often. The framework handles
/// spawning, the select loop, and shutdown coordination via the broadcast channel.
///
/// Return [`ControlFlow::Break`] from [`poll`](PollingWatcher::poll) to stop the watcher
/// (e.g. when the watched resource disappears).
pub trait PollingWatcher: Send + 'static {
    /// Human-readable name for log messages (e.g. ".env", "filesystem").
    fn label(&self) -> &'static str;

    /// How often to call [`poll`](PollingWatcher::poll).
    fn poll_interval(&self) -> Duration;

    /// Check the watched condition and take action.
    ///
    /// Called once per [`poll_interval`](PollingWatcher::poll_interval).
    /// Return `ControlFlow::Continue(())` to keep watching,
    /// or `ControlFlow::Break(())` to stop this watcher.
    ///
    /// The returned future must be `Send` so it can run inside `tokio::spawn`.
    fn poll(&mut self) -> impl Future<Output = ControlFlow<()>> + Send;
}

/// Spawn a polling watcher as a background task.
///
/// The task calls `watcher.poll()` at the configured interval and stops
/// when either `shutdown_rx` fires or `poll()` returns `Break`.
pub fn spawn_polling_watcher<W: PollingWatcher>(
    mut watcher: W,
    mut shutdown_rx: broadcast::Receiver<Shutdown>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(Shutdown::Stop) | Err(_) => {
                            debug!("{} watcher stopping.", watcher.label());
                            break;
                        }
                    }
                }
                () = tokio::time::sleep(watcher.poll_interval()) => {
                    if watcher.poll().await.is_break() {
                        break;
                    }
                }
            }
        }
    });
}
