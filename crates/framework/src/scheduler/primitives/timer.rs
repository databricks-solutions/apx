//! [`Timer`] — deadline-based awaitable timer.

use pyo3::prelude::*;

use super::future::Future;

/// A Rust-backed awaitable timer.
///
/// Wraps a [`Future`] that is resolved after the specified delay.
/// For zero-delay timers, the future is resolved immediately.
/// For non-zero delays, a background thread sleeps and resolves the future.
///
/// The awaitable protocol delegates to the inner `Future`, so the
/// Rust scheduler can classify and suspend on it properly.
#[pyclass(module = "apx._core")]
pub struct Timer {
    inner: Py<Future>,
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer").finish_non_exhaustive()
    }
}

#[pymethods]
impl Timer {
    /// Create a new timer that fires after `delay_secs` seconds.
    #[new]
    pub(crate) fn new(py: Python<'_>, delay_secs: f64) -> PyResult<Self> {
        let inner = if delay_secs <= 0.0 {
            Py::new(py, Future::resolved(py.None()))?
        } else {
            let (future, tx) = Future::with_channel();
            let inner = Py::new(py, future)?;
            let duration = std::time::Duration::from_secs_f64(delay_secs);

            // Prefer tokio timer wheel (efficient, no OS thread per timer).
            // Fall back to raw thread + sleep if no tokio runtime available.
            let handle = crate::scheduler::with_tokio_handle(tokio::runtime::Handle::clone);
            if let Some(handle) = handle {
                handle.spawn(async move {
                    tokio::time::sleep(duration).await;
                    Python::attach(|py| {
                        let _ = tx.send(py.None());
                    });
                });
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(duration);
                    Python::attach(|py| {
                        let _ = tx.send(py.None());
                    });
                });
            }
            inner
        };
        Ok(Self { inner })
    }

    /// Python awaitable protocol: delegate to the inner Future.
    fn __await__(&self, py: Python<'_>) -> Py<Future> {
        self.inner.clone_ref(py)
    }

    /// Check whether the timer has fired.
    pub(crate) fn done(&self, py: Python<'_>) -> bool {
        self.inner.borrow(py).done()
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn timer_zero_delay_fires_immediately() {
        crate::with_py(|py| {
            let timer = Timer::new(py, 0.0).unwrap();
            // Zero-delay timer wraps a resolved Future.
            assert!(timer.done(py));
        });
    }

    #[test]
    fn timer_future_delay_not_ready() {
        crate::with_py(|py| {
            let timer = Timer::new(py, 999.0).unwrap();
            assert!(!timer.done(py));
        });
    }
}
