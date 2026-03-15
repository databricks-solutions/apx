//! Driver pool manager with dynamic scaling.
//!
//! Manages N driver threads that consume coroutine work from a shared
//! `crossbeam::channel`. Supports adding/removing threads at runtime.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

use super::channel::{DriverReceiver, DriverSender, create_driver_channel};
use super::thread::{DriverConfig, SharedDriverState, run};

/// A pool of driver threads that process coroutines concurrently.
///
/// Supports dynamic scaling: [`add_threads`](Self::add_threads) /
/// [`remove_threads`](Self::remove_threads) at runtime.
/// The crossbeam channel is shared across all threads — new threads
/// immediately start consuming from it, removed threads drain gracefully
/// via `Shutdown` sentinels.
pub struct DriverPool {
    /// Active driver threads (guarded by mutex for dynamic scaling).
    threads: Mutex<Vec<DriverThread>>,
    /// Monotonically increasing thread ID counter.
    next_id: AtomicUsize,
    /// Shared sender (cloneable — handed to `EventLoopHandle`, `ReadyQueue`).
    sender: DriverSender,
    /// Shared receiver (cloneable — each new thread gets a clone).
    receiver: DriverReceiver,
    /// Shared config for spawning new threads (no per-thread state).
    shared: Arc<SharedDriverState>,
}

impl std::fmt::Debug for DriverPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverPool")
            .field("thread_count", &self.thread_count())
            .finish_non_exhaustive()
    }
}

/// Handle for a single driver thread.
struct DriverThread {
    id: usize,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for DriverThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverThread")
            .field("id", &self.id)
            .finish()
    }
}

impl DriverPool {
    /// Start the pool with `n` initial driver threads.
    pub fn start(n: usize, shared: SharedDriverState) -> Self {
        let (sender, receiver) = create_driver_channel();
        let shared = Arc::new(shared);

        let pool = Self {
            threads: Mutex::new(Vec::with_capacity(n)),
            next_id: AtomicUsize::new(0),
            sender,
            receiver,
            shared,
        };

        pool.add_threads(n);
        pool
    }

    /// Spawn `n` additional driver threads (dynamic scale-up).
    ///
    /// Returns the IDs of the newly spawned threads.
    pub fn add_threads(&self, n: usize) -> Vec<usize> {
        let mut ids = Vec::with_capacity(n);
        let mut threads = self
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for _ in 0..n {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let config = DriverConfig {
                id,
                receiver: self.receiver.clone(),
                shared: Arc::clone(&self.shared),
            };

            let thread_name = format!("apx-driver-{id}");
            let handle = match std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || run(config))
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!(error = %e, thread = %thread_name, "failed to spawn driver thread");
                    continue;
                }
            };

            threads.push(DriverThread { id, handle });
            ids.push(id);
        }

        tracing::info!(count = n, total = threads.len(), "driver threads added");
        ids
    }

    /// Remove `n` driver threads by sending `Shutdown` sentinels.
    ///
    /// Threads drain their current item and exit gracefully.
    /// The threads are joined in a background thread to avoid blocking the caller.
    ///
    /// **Note:** Sentinels are consumed by any idle thread, not necessarily the
    /// threads removed from the internal list. The join thread may block until
    /// the targeted threads finish their current work item. This is acceptable
    /// for a scaling API — the pool converges to the correct size.
    pub fn remove_threads(&self, n: usize) {
        // Send shutdown sentinels — any idle thread picks one up and exits.
        for _ in 0..n {
            let _ = self.sender.send_shutdown();
        }

        // Collect the handles of threads to join.
        let handles: Vec<DriverThread> = {
            let mut threads = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            // We can't know which threads will pick up the sentinels, so
            // we remove from the end (LIFO).
            let drain_count = n.min(threads.len());
            let split_at = threads.len() - drain_count;
            threads.split_off(split_at)
        };

        // Join in a background thread so we don't block the caller.
        if !handles.is_empty() {
            let count = handles.len();
            if let Err(e) = std::thread::Builder::new()
                .name("apx-driver-join".to_owned())
                .spawn(move || {
                    for dt in handles {
                        let _ = dt.handle.join();
                    }
                    tracing::info!(count, "driver threads removed");
                })
            {
                tracing::warn!(error = %e, "failed to spawn join thread");
            }
        }
    }

    /// Current number of active driver threads.
    pub fn thread_count(&self) -> usize {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Get a clone of the sender for submitting work.
    pub fn sender(&self) -> DriverSender {
        self.sender.clone()
    }

    /// Shut down all driver threads and join them.
    pub fn stop(&self) {
        let threads: Vec<DriverThread> = {
            let mut guard = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };

        // Send one shutdown sentinel per thread.
        for _ in &threads {
            let _ = self.sender.send_shutdown();
        }

        // Join all threads.
        for dt in threads {
            let _ = dt.handle.join();
        }

        tracing::info!("driver pool stopped");
    }
}

impl Drop for DriverPool {
    fn drop(&mut self) {
        self.stop();
    }
}
