//! GIL-free event loop wake strategy.
//!
//! On Unix: pipe + `add_reader` — the tokio thread writes a byte to a pipe fd,
//! the event loop's selector sees the fd become readable and fires the drainer.
//! No GIL, no Python, no locks from the tokio thread.
//!
//! On Windows: fallback to `call_soon_threadsafe` — `ProactorEventLoop` doesn't
//! support `add_reader`.

use std::sync::Arc;

use pyo3::prelude::*;

/// Strategy for waking the event loop from a tokio thread.
///
/// On Unix: pipe + `add_reader` — no GIL needed on the wake path.
/// On Windows: `call_soon_threadsafe` fallback — `ProactorEventLoop`
/// doesn't support `add_reader`.
pub enum WakeStrategy {
    /// Unix: write a byte to a pipe fd. The event loop's selector sees the
    /// fd become readable and fires the drainer callback.
    #[cfg(unix)]
    Pipe {
        /// Write end — `File::write(&[1])` wakes the selector.
        write_end: std::fs::File,
        /// Read-end raw fd — registered with `event_loop.add_reader()`.
        read_fd: std::os::unix::io::RawFd,
    },
    /// Fallback: acquire GIL and call `loop.call_soon_threadsafe(drainer)`.
    /// Used on Windows or when `add_reader` registration fails.
    Gil {
        call_soon_threadsafe: Py<PyAny>,
        drainer_ref: Py<PyAny>,
    },
}

impl WakeStrategy {
    /// Create the best available wake strategy for this platform.
    ///
    /// On Unix, tries pipe + `add_reader` first. Falls back to GIL-based
    /// `call_soon_threadsafe` if pipe setup fails or on Windows.
    pub fn create(
        py: Python<'_>,
        event_loop: &Bound<'_, PyAny>,
        drainer_obj: &Py<PyAny>,
    ) -> Result<Self, String> {
        #[cfg(unix)]
        {
            match Self::try_create_pipe(py, event_loop, drainer_obj) {
                Ok(strategy) => return Ok(strategy),
                Err(e) => {
                    tracing::warn!(error = %e, "pipe wake unavailable, falling back to GIL");
                }
            }
        }
        Self::create_gil_fallback(py, event_loop, drainer_obj)
    }

    /// Wake the event loop.
    ///
    /// Pipe: pure Rust write, no GIL. Gil: acquires GIL via `Python::attach`.
    pub fn wake(&self) {
        match self {
            #[cfg(unix)]
            Self::Pipe { write_end, .. } => {
                // Pure Rust — no GIL, no Python.
                // `std::io::Write for &File` is implemented in std, so concurrent
                // writes from multiple tokio threads are safe (kernel serializes).
                let _ = std::io::Write::write(&mut &*write_end, &[0x01]);
            }
            Self::Gil {
                call_soon_threadsafe,
                drainer_ref,
            } => {
                Python::attach(|py| {
                    let _ = call_soon_threadsafe.call1(py, (drainer_ref,));
                });
            }
        }
    }

    /// Return the read-end fd if using pipe wake (for drain + `remove_reader`).
    #[cfg(unix)]
    pub fn read_fd(&self) -> Option<std::os::unix::io::RawFd> {
        match self {
            Self::Pipe { read_fd, .. } => Some(*read_fd),
            Self::Gil { .. } => None,
        }
    }

    /// GIL-based fallback — always available on all platforms.
    fn create_gil_fallback(
        py: Python<'_>,
        event_loop: &Bound<'_, PyAny>,
        drainer_obj: &Py<PyAny>,
    ) -> Result<Self, String> {
        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .map_err(|e| format!("missing call_soon_threadsafe: {e}"))?
            .unbind();
        tracing::info!("using GIL-based call_soon_threadsafe wake strategy");
        Ok(Self::Gil {
            call_soon_threadsafe,
            drainer_ref: drainer_obj.clone_ref(py),
        })
    }

    /// Try to create a pipe-based wake strategy (Unix only).
    #[cfg(unix)]
    fn try_create_pipe(
        py: Python<'_>,
        event_loop: &Bound<'_, PyAny>,
        drainer_obj: &Py<PyAny>,
    ) -> Result<Self, String> {
        use std::os::unix::io::{FromRawFd, OwnedFd};

        let os_mod = py.import(c"os").map_err(|e| format!("import os: {e}"))?;

        // os.pipe() → (read_fd, write_fd)
        let pipe_tuple = os_mod
            .call_method0(c"pipe")
            .map_err(|e| format!("os.pipe(): {e}"))?;
        let read_fd: i32 = pipe_tuple
            .get_item(0)
            .map_err(|e| format!("pipe[0]: {e}"))?
            .extract()
            .map_err(|e| format!("pipe[0] extract: {e}"))?;
        let write_fd: i32 = pipe_tuple
            .get_item(1)
            .map_err(|e| format!("pipe[1]: {e}"))?
            .extract()
            .map_err(|e| format!("pipe[1] extract: {e}"))?;

        // Set both ends non-blocking
        os_mod
            .call_method1(c"set_blocking", (read_fd, false))
            .map_err(|e| format!("set_blocking(read_fd): {e}"))?;
        os_mod
            .call_method1(c"set_blocking", (write_fd, false))
            .map_err(|e| format!("set_blocking(write_fd): {e}"))?;

        // Register read end with event loop selector
        if let Err(e) = event_loop.call_method1(c"add_reader", (read_fd, drainer_obj)) {
            // add_reader failed — close fds and fall back.
            let _ = os_mod.call_method1(c"close", (read_fd,));
            let _ = os_mod.call_method1(c"close", (write_fd,));
            return Err(format!("add_reader: {e}"));
        }

        // SAFETY: `write_fd` is a valid fd from `os.pipe()`. We take ownership —
        // Python won't close it (we extracted it as an int, not a file object).
        // `OwnedFd` manages the lifetime from here.
        #[allow(unsafe_code)]
        let write_end = unsafe { std::fs::File::from(OwnedFd::from_raw_fd(write_fd)) };

        tracing::info!(read_fd, write_fd, "using pipe-based GIL-free wake strategy");
        Ok(Self::Pipe { write_end, read_fd })
    }
}

impl std::fmt::Debug for WakeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Pipe { read_fd, .. } => f
                .debug_struct("WakeStrategy::Pipe")
                .field("read_fd", read_fd)
                .finish(),
            Self::Gil { .. } => f.debug_struct("WakeStrategy::Gil").finish_non_exhaustive(),
        }
    }
}

/// Create an `Arc<WakeStrategy>` for sharing across handles.
pub fn create_wake_strategy(
    py: Python<'_>,
    event_loop: &Bound<'_, PyAny>,
    drainer_obj: &Py<PyAny>,
) -> Result<Arc<WakeStrategy>, String> {
    WakeStrategy::create(py, event_loop, drainer_obj).map(Arc::new)
}
