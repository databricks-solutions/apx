//! Core awaitable primitives for the Rust-driven scheduler.
//!
//! [`Future`] is the foundational awaitable — it implements the Python
//! awaitable protocol so that both asyncio and our Rust coroutine driver
//! can drive it.
//!
//! Additional primitives:
//! - [`Event`] — async event flag (wraps `tokio::sync::Notify`)
//! - [`Timer`] — deadline-based awaitable timer
//! - [`CancelToken`] — structured cancellation flag
//! - [`Lock`] — async mutex (wraps `tokio::sync::Mutex`)
//! - [`Semaphore`] — counting semaphore (wraps `tokio::sync::Semaphore`)
//! - [`BlockingTask`] — awaitable for work spawned on a blocking thread
//! - [`IoHandle`] — stub for future I/O integration

// All types in this module are `#[pyclass]` — PyO3 manages their identity
// semantics, so `Copy` is intentionally not implemented.
#![allow(missing_copy_implementations)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// Future
// ---------------------------------------------------------------------------

/// A Rust-backed Python awaitable.
///
/// Implements the Python awaitable protocol (`__await__` + `__next__`) and
/// can be resolved from Rust via [`set_result`](Future::set_result) or
/// [`set_exception`](Future::set_exception), or through a
/// [`oneshot::Sender`] returned by [`Future::with_channel`].
///
/// # Awaitable protocol
///
/// Python's `await` desugars to calling `__await__()` to get an iterator,
/// then repeatedly calling `__next__()` on it. When the result is ready,
/// `__next__` raises `StopIteration(value)`. Until then it yields `self`
/// so the Rust scheduler can classify and suspend on the future.
#[pyclass(module = "apx._core", weakref)]
pub struct Future {
    /// Oneshot receiver for results arriving from Rust.
    rx: Option<oneshot::Receiver<Py<PyAny>>>,
    /// Stored result (once resolved).
    inner_result: Option<PyResult<Py<PyAny>>>,
    /// Python callbacks registered via `add_done_callback`.
    wakers: Vec<Py<PyAny>>,
}

impl Future {
    /// Create a `Future` paired with a [`oneshot::Sender`] for resolution.
    ///
    /// The sender can be moved to any thread; sending a value through it
    /// will resolve the future on the next `__next__` poll.
    pub fn with_channel() -> (Self, oneshot::Sender<Py<PyAny>>) {
        let (tx, rx) = oneshot::channel();
        let future = Self {
            rx: Some(rx),
            inner_result: None,
            wakers: Vec::new(),
        };
        (future, tx)
    }

    /// Create a `Future` that is already resolved with the given value.
    pub fn resolved(value: Py<PyAny>) -> Self {
        Self {
            rx: None,
            inner_result: Some(Ok(value)),
            wakers: Vec::new(),
        }
    }

    /// Invoke all registered done callbacks with `self` as the argument.
    fn fire_wakers(&mut self, py: Python<'_>, slf: &Py<Self>) {
        for cb in self.wakers.drain(..) {
            // Best-effort: swallow exceptions from callbacks (matches asyncio behaviour).
            if let Err(e) = cb.call1(py, (slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        }
    }

    /// Raise `StopIteration(value)` or re-raise the stored exception.
    fn raise_result(py: Python<'_>, result: &PyResult<Py<PyAny>>) -> PyErr {
        match result {
            Ok(value) => pyo3::exceptions::PyStopIteration::new_err((value.clone_ref(py),)),
            Err(err) => err.clone_ref(py),
        }
    }
}

impl std::fmt::Debug for Future {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Future")
            .field("done", &self.inner_result.is_some())
            .field("wakers", &self.wakers.len())
            .finish()
    }
}

#[pymethods]
impl Future {
    /// Python awaitable protocol: return self as the iterator.
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Python iterator protocol (also needed for `__await__`).
    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Iterator protocol: poll for the result.
    ///
    /// - If the result is ready, raises `StopIteration(value)`.
    /// - If an exception was stored, re-raises it.
    /// - Otherwise yields `self` so the Rust scheduler can classify it as
    ///   `Future` and suspend (attach a done-callback) instead of
    ///   busy-looping on `YieldNone`.
    fn __next__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let mut this = slf.borrow_mut(py);

        // Already resolved — raise immediately.
        if let Some(ref result) = this.inner_result {
            return Err(Self::raise_result(py, result));
        }

        // Try to receive from the oneshot channel.
        if let Some(ref mut rx) = this.rx {
            match rx.try_recv() {
                Ok(value) => {
                    this.inner_result = Some(Ok(value.clone_ref(py)));
                    this.rx = None;
                    let stop = pyo3::exceptions::PyStopIteration::new_err((value,));
                    // Drop mutable borrow before firing wakers.
                    drop(this);
                    slf.borrow_mut(py).fire_wakers(py, &slf);
                    Err(stop)
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    // Not ready yet — yield self so the scheduler can suspend.
                    drop(this);
                    Ok(slf.into_any())
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    // Sender dropped without sending — treat as cancellation.
                    let err = pyo3::exceptions::PyRuntimeError::new_err(
                        "Future: sender dropped without producing a result",
                    );
                    this.inner_result = Some(Err(err.clone_ref(py)));
                    this.rx = None;
                    drop(this);
                    slf.borrow_mut(py).fire_wakers(py, &slf);
                    Err(err)
                }
            }
        } else {
            // No channel and no result — should not happen, but handle gracefully.
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Future: no channel and no result",
            ))
        }
    }

    /// Resolve the future with a value.
    ///
    /// Any registered done-callbacks are invoked immediately.
    fn set_result(slf: Py<Self>, py: Python<'_>, value: Py<PyAny>) -> PyResult<()> {
        {
            let mut this = slf.borrow_mut(py);
            if this.inner_result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            this.inner_result = Some(Ok(value));
            this.rx = None;
        }
        // Fire wakers outside the borrow.
        slf.borrow_mut(py).fire_wakers(py, &slf);
        Ok(())
    }

    /// Resolve the future with an exception.
    ///
    /// The exception object is stored and re-raised on the next `__next__` call.
    fn set_exception(slf: Py<Self>, py: Python<'_>, exc: Py<PyAny>) -> PyResult<()> {
        {
            let mut this = slf.borrow_mut(py);
            if this.inner_result.is_some() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "Future: result already set",
                ));
            }
            let err = PyErr::from_value(exc.into_bound(py));
            this.inner_result = Some(Err(err));
            this.rx = None;
        }
        slf.borrow_mut(py).fire_wakers(py, &slf);
        Ok(())
    }

    /// Get the result if available. Raises if not yet resolved or if an exception was stored.
    #[pyo3(name = "result")]
    pub(crate) fn get_result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.inner_result {
            Some(Ok(value)) => Ok(value.clone_ref(py)),
            Some(Err(err)) => Err(err.clone_ref(py)),
            None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "Future: result not yet available",
            )),
        }
    }

    /// Check whether the future has been resolved.
    pub(crate) fn done(&self) -> bool {
        self.inner_result.is_some()
    }

    /// Return the stored exception, if the future resolved with an error.
    pub(crate) fn exception(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        match &self.inner_result {
            Some(Err(err)) => Some(err.value(py).clone().unbind().into()),
            _ => None,
        }
    }

    /// Register a callback to be invoked when the future resolves.
    ///
    /// If the future is already resolved, the callback is invoked immediately.
    fn add_done_callback(slf: Py<Self>, py: Python<'_>, callback: Py<PyAny>) -> PyResult<()> {
        let done = slf.borrow(py).inner_result.is_some();
        if done {
            // Already done — fire immediately.
            if let Err(e) = callback.call1(py, (&slf,)) {
                tracing::warn!(error = %e, "Future done-callback raised");
            }
        } else {
            slf.borrow_mut(py).wakers.push(callback);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Event — async event flag (wraps Arc<tokio::sync::Notify>)
// ---------------------------------------------------------------------------

/// A Rust-backed async event flag, analogous to `asyncio.Event`.
///
/// `wait()` returns a [`EventWaiter`] that wraps a [`Future`].
/// When `set()` is called, all pending waiter futures are resolved,
/// causing the Rust scheduler to resume waiting coroutines via
/// done-callbacks instead of busy-polling.
#[pyclass(module = "apx._core")]
pub struct Event {
    is_set: AtomicBool,
    /// Pending waiter senders — resolved when `set()` is called.
    pending: std::sync::Mutex<Vec<oneshot::Sender<Py<PyAny>>>>,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("set", &self.is_set.load(Ordering::Relaxed))
            .finish()
    }
}

#[pymethods]
impl Event {
    #[new]
    pub(crate) fn new() -> Self {
        Self {
            is_set: AtomicBool::new(false),
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Set the event flag and resolve all pending waiter futures.
    fn set(&self) {
        self.is_set.store(true, Ordering::Release);
        let senders = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pending)
        };
        Python::attach(|py| {
            for tx in senders {
                let _ = tx.send(py.None());
            }
        });
    }

    /// Check whether the event is currently set.
    pub(crate) fn is_set(&self) -> bool {
        self.is_set.load(Ordering::Acquire)
    }

    /// Reset the event flag.
    fn clear(&self) {
        self.is_set.store(false, Ordering::Release);
    }

    /// Return an awaitable that resolves when the event is set.
    fn wait(&self, py: Python<'_>) -> PyResult<EventWaiter> {
        if self.is_set.load(Ordering::Acquire) {
            let inner = Py::new(py, Future::resolved(py.None()))?;
            return Ok(EventWaiter { inner });
        }
        let (future, tx) = Future::with_channel();
        let inner = Py::new(py, future)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        // Double-check after acquiring lock — event may have been set.
        if self.is_set.load(Ordering::Acquire) {
            drop(pending);
            let _ = tx.send(py.None());
        } else {
            pending.push(tx);
        }
        Ok(EventWaiter { inner })
    }
}

/// Awaitable returned by [`Event::wait`].
///
/// Wraps a [`Future`] that resolves when the parent event is set.
/// The scheduler can classify and suspend on the inner future properly.
#[pyclass(module = "apx._core")]
pub struct EventWaiter {
    inner: Py<Future>,
}

impl std::fmt::Debug for EventWaiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventWaiter").finish_non_exhaustive()
    }
}

#[pymethods]
impl EventWaiter {
    /// Python awaitable protocol: delegate to the inner Future.
    fn __await__(&self, py: Python<'_>) -> Py<Future> {
        self.inner.clone_ref(py)
    }
}

// ---------------------------------------------------------------------------
// Timer — deadline-based awaitable
// ---------------------------------------------------------------------------

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
    fn new() -> Self {
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
    fn new(permits: u32) -> Self {
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

// ---------------------------------------------------------------------------
// BlockingTask — awaitable for spawn_blocking work
// ---------------------------------------------------------------------------

/// Awaitable representing work spawned on a blocking thread.
///
/// Created via [`spawn_blocking`]. Implements the Python awaitable protocol:
/// polls the internal `oneshot::Receiver` and raises `StopIteration(result)`
/// when the blocking work completes.
#[pyclass(module = "apx._core")]
pub struct BlockingTask {
    rx: Option<oneshot::Receiver<PyResult<Py<PyAny>>>>,
    result: Option<PyResult<Py<PyAny>>>,
}

impl BlockingTask {
    /// Create a `BlockingTask` wired to the given oneshot receiver.
    ///
    /// Used by adapters that spawn actual blocking work on a separate thread
    /// and need to hand back an awaitable.
    pub(crate) fn with_receiver(rx: oneshot::Receiver<PyResult<Py<PyAny>>>) -> Self {
        Self {
            rx: Some(rx),
            result: None,
        }
    }
}

impl std::fmt::Debug for BlockingTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingTask")
            .field("done", &self.result.is_some())
            .finish()
    }
}

#[pymethods]
impl BlockingTask {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let mut this = slf.borrow_mut(py);

        // Already resolved — raise immediately.
        if let Some(ref result) = this.result {
            return match result {
                Ok(value) => Err(pyo3::exceptions::PyStopIteration::new_err((
                    value.clone_ref(py),
                ))),
                Err(err) => Err(err.clone_ref(py)),
            };
        }

        // Try to receive from the oneshot channel.
        if let Some(ref mut rx) = this.rx {
            match rx.try_recv() {
                Ok(Ok(value)) => {
                    let stop = pyo3::exceptions::PyStopIteration::new_err((value.clone_ref(py),));
                    this.result = Some(Ok(value));
                    this.rx = None;
                    Err(stop)
                }
                Ok(Err(err)) => {
                    let py_err = err.clone_ref(py);
                    this.result = Some(Err(err));
                    this.rx = None;
                    Err(py_err)
                }
                Err(oneshot::error::TryRecvError::Empty) => Ok(py.None()),
                Err(oneshot::error::TryRecvError::Closed) => {
                    let err = pyo3::exceptions::PyRuntimeError::new_err(
                        "BlockingTask: sender dropped without producing a result",
                    );
                    this.result = Some(Err(err.clone_ref(py)));
                    this.rx = None;
                    Err(err)
                }
            }
        } else {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "BlockingTask: no channel and no result",
            ))
        }
    }

    /// Check whether the blocking task has completed.
    fn done(&self) -> bool {
        self.result.is_some()
    }
}

/// Spawn a Python callable on a blocking thread and return an awaitable.
///
/// NOTE: For now, this creates the structure with a oneshot channel. The
/// actual `spawn_blocking` integration (calling the callable on a blocking
/// thread with GIL acquired) happens in the driver/adapters layer.
#[pyfunction]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "registered as Python function when blocking dispatch is wired"
    )
)]
pub fn spawn_blocking(_py: Python<'_>, _callable: Py<PyAny>) -> PyResult<BlockingTask> {
    let (_tx, rx) = oneshot::channel();
    Ok(BlockingTask {
        rx: Some(rx),
        result: None,
    })
}

// ---------------------------------------------------------------------------
// IoHandle — stub for future I/O integration
// ---------------------------------------------------------------------------

/// Placeholder for future I/O integration.
///
/// Will wrap `AsyncFd` or `TcpStream` in a future phase. Currently a
/// forward-compatibility stub.
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct IoHandle {
    _private: (),
}

#[pymethods]
impl IoHandle {
    #[new]
    fn new() -> Self {
        Self { _private: () }
    }

    #[allow(
        clippy::unused_self,
        reason = "__repr__ requires &self per Python protocol"
    )]
    fn __repr__(&self) -> &'static str {
        "IoHandle(stub)"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    // -- Future tests ---------------------------------------------------

    #[test]
    fn with_channel_creates_pair() {
        let (future, _tx) = Future::with_channel();
        assert!(!future.done());
        assert!(future.rx.is_some());
        assert!(future.inner_result.is_none());
    }

    #[test]
    fn resolved_is_immediately_done() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            assert!(future.done());
            assert!(future.rx.is_none());
        });
    }

    #[test]
    fn debug_format() {
        let (future, _tx) = Future::with_channel();
        let dbg = format!("{future:?}");
        assert!(dbg.contains("Future"));
        assert!(dbg.contains("done: false"));
        assert!(dbg.contains("wakers: 0"));
    }

    #[test]
    fn double_set_result_errors() {
        crate::with_py(|py| {
            let future = Future::resolved(py.None());
            let slf = Py::new(py, future).unwrap();
            let err = Future::set_result(slf, py, py.None());
            assert!(err.is_err());
        });
    }

    // -- Event tests ----------------------------------------------------

    #[test]
    fn event_starts_unset() {
        let event = Event::new();
        assert!(!event.is_set());
    }

    #[test]
    fn event_set_and_clear() {
        let event = Event::new();
        event.set();
        assert!(event.is_set());
        event.clear();
        assert!(!event.is_set());
    }

    // -- Timer tests --------------------------------------------------------

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

    // -- BlockingTask tests -------------------------------------------------

    #[test]
    fn blocking_task_not_done_initially() {
        crate::with_py(|py| {
            let task = spawn_blocking(py, py.None()).unwrap();
            assert!(!task.done());
        });
    }

    // -- IoHandle tests -----------------------------------------------------

    #[test]
    fn io_handle_repr() {
        let handle = IoHandle::new();
        assert_eq!(handle.__repr__(), "IoHandle(stub)");
    }
}
