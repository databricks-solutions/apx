//! Synchronization primitives: [`CancelToken`], [`Lock`], [`Semaphore`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;

// ---------------------------------------------------------------------------
// CancelToken — structured cancellation
// ---------------------------------------------------------------------------

/// A structured cancellation token.
///
/// Can be shared across tasks; calling [`cancel`](CancelToken::cancel) sets
/// the flag, and [`check`](CancelToken::check) raises `asyncio.CancelledError`
/// if cancelled.
#[pyclass(module = "apx._core")]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .finish()
    }
}

#[pymethods]
impl CancelToken {
    #[new]
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Cancel the token.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check whether the token has been cancelled.
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Raise `asyncio.CancelledError` if cancelled, otherwise do nothing.
    fn check(&self, py: Python<'_>) -> PyResult<()> {
        if self.cancelled.load(Ordering::Acquire) {
            let cancelled_error = py.import(c"asyncio")?.getattr(c"CancelledError")?;
            Err(PyErr::from_value(cancelled_error.call0()?))
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Lock — async mutex (wraps Arc<tokio::sync::Mutex<()>>)
// ---------------------------------------------------------------------------

/// A Rust-backed async mutex, analogous to `asyncio.Lock`.
///
/// Uses `tokio::sync::Mutex` internally. The `acquire()` method returns an
/// awaitable [`LockGuardFuture`] that resolves to a [`LockGuard`].
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct Lock {
    inner: Arc<tokio::sync::Mutex<()>>,
}

#[pymethods]
impl Lock {
    #[new]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Return an awaitable that resolves to a [`LockGuard`] once acquired.
    fn acquire(slf: Py<Self>, py: Python<'_>) -> LockGuardFuture {
        let this = slf.borrow(py);
        LockGuardFuture {
            mutex: Arc::clone(&this.inner),
        }
    }

    /// Check whether the lock is currently held.
    fn locked(&self) -> bool {
        self.inner.try_lock().is_err()
    }
}

/// Awaitable returned by [`Lock::acquire`].
///
/// Implements the Python awaitable protocol: tries `try_lock` on each poll.
/// When acquired, raises `StopIteration(guard)` with a [`LockGuard`].
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct LockGuardFuture {
    mutex: Arc<tokio::sync::Mutex<()>>,
}

#[pymethods]
impl LockGuardFuture {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match Arc::clone(&self.mutex).try_lock_owned() {
            Ok(guard) => {
                let py_guard = Py::new(py, LockGuard { guard: Some(guard) })?;
                Err(pyo3::exceptions::PyStopIteration::new_err((py_guard,)))
            }
            Err(_) => Ok(py.None()),
        }
    }
}

/// RAII guard for a [`Lock`].
///
/// Dropping or calling [`release`](LockGuard::release) releases the lock.
/// Also supports the context-manager protocol (`with guard: ...`).
#[pyclass(module = "apx._core")]
pub struct LockGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("held", &self.guard.is_some())
            .finish()
    }
}

#[pymethods]
impl LockGuard {
    /// Release the lock explicitly.
    fn release(&mut self) {
        self.guard.take();
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<Py<PyAny>>,
        _exc_val: Option<Py<PyAny>>,
        _exc_tb: Option<Py<PyAny>>,
    ) -> bool {
        self.guard.take();
        // Do not suppress exceptions.
        false
    }
}

// ---------------------------------------------------------------------------
// Semaphore — counting semaphore (wraps Arc<tokio::sync::Semaphore>)
// ---------------------------------------------------------------------------

/// A Rust-backed counting semaphore, analogous to `asyncio.Semaphore`.
///
/// Uses `tokio::sync::Semaphore` internally. The `acquire()` method returns
/// an awaitable [`SemaphoreAcquire`] that resolves to a
/// [`SemaphorePermit`].
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct Semaphore {
    inner: Arc<tokio::sync::Semaphore>,
}

#[pymethods]
impl Semaphore {
    #[new]
    pub fn new(permits: u32) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Semaphore::new(permits as usize)),
        }
    }

    /// Return an awaitable that resolves to a [`SemaphorePermit`].
    fn acquire(slf: Py<Self>, py: Python<'_>) -> SemaphoreAcquire {
        let this = slf.borrow(py);
        SemaphoreAcquire {
            semaphore: Arc::clone(&this.inner),
        }
    }

    /// Return the number of permits currently available.
    fn available_permits(&self) -> u32 {
        self.inner.available_permits() as u32
    }
}

/// Awaitable returned by [`Semaphore::acquire`].
///
/// Implements the Python awaitable protocol: tries `try_acquire_owned` on
/// each poll. When acquired, raises `StopIteration(permit)`.
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct SemaphoreAcquire {
    semaphore: Arc<tokio::sync::Semaphore>,
}

#[pymethods]
impl SemaphoreAcquire {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => {
                let py_permit = Py::new(
                    py,
                    SemaphorePermit {
                        permit: Some(permit),
                    },
                )?;
                Err(pyo3::exceptions::PyStopIteration::new_err((py_permit,)))
            }
            Err(_) => Ok(py.None()),
        }
    }
}

/// RAII permit for a [`Semaphore`].
///
/// Dropping or calling [`release`](SemaphorePermit::release) returns the
/// permit to the semaphore.
#[pyclass(module = "apx._core")]
pub struct SemaphorePermit {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl std::fmt::Debug for SemaphorePermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemaphorePermit")
            .field("held", &self.permit.is_some())
            .finish()
    }
}

#[pymethods]
impl SemaphorePermit {
    /// Release the permit explicitly (returns it to the semaphore).
    fn release(&mut self) {
        self.permit.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- CancelToken tests --------------------------------------------------

    #[test]
    fn cancel_token_starts_uncancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_token_cancel() {
        let token = CancelToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_token_check_raises() {
        crate::with_py(|py| {
            let token = CancelToken::new();
            assert!(token.check(py).is_ok());
            token.cancel();
            assert!(token.check(py).is_err());
        });
    }

    // -- Lock tests -----------------------------------------------------

    #[test]
    fn lock_starts_unlocked() {
        let lock = Lock::new();
        assert!(!lock.locked());
    }

    // -- Semaphore tests ------------------------------------------------

    #[test]
    fn semaphore_available_permits() {
        let sem = Semaphore::new(5);
        assert_eq!(sem.available_permits(), 5);
    }
}
