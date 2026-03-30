//! Cross-thread dispatch channels for the zero-GIL 3-thread architecture.
//!
//! Thread 1 (tokio) pushes [`RequestSlot`] into the inbound channel and
//! signals the asyncio thread via [`Wakeup`]. Thread 2 (Python/asyncio)
//! drains requests, runs the ASGI app, and pushes [`OutboundSlot`] into
//! the outbound channel. Thread 3 relays responses back to tokio via
//! oneshot senders.
//!
//! All types in this module are pure Rust — no `Py<T>`, no GIL.

use bytes::Bytes;
use http::header::HeaderMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};

use crate::transport::types::ProtocolVersion;

// ── RequestSlot ──────────────────────────────────────────────────────────

/// Request flowing from Thread 1 (tokio) → Thread 2 (asyncio). Pure Rust.
#[derive(Debug)]
pub struct RequestSlot {
    /// HTTP method.
    pub method: http::Method,
    /// Request path (without query string).
    pub path: String,
    /// Raw path bytes for ASGI `raw_path`.
    pub raw_path: Bytes,
    /// Raw query string bytes.
    pub query_string: Bytes,
    /// HTTP headers.
    pub headers: HeaderMap,
    /// Pre-collected request body.
    pub body: Bytes,
    /// HTTP protocol version.
    pub protocol: ProtocolVersion,
    /// Client socket address (if available).
    pub client_addr: Option<SocketAddr>,
    /// Server socket address.
    pub server_addr: SocketAddr,
    /// Trace context extracted from the active OTEL span on the tokio thread.
    pub trace_context: Option<crate::telemetry::context::TraceContext>,
    /// Timestamp when the slot was created (for pickup_delay measurement).
    pub created_at: std::time::Instant,
    /// Thread 1 awaits this for the response.
    pub response_tx: oneshot::Sender<ResponseData>,
}

// ── ResponseData ─────────────────────────────────────────────────────────

/// Response flowing from Thread 2 → Thread 3 → Thread 1.
///
/// Uses an unbounded mpsc channel for the body to unify streaming and
/// non-streaming responses under a single code path.
#[derive(Debug)]
pub struct ResponseData {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as raw byte pairs (name, value).
    pub headers: Vec<(Bytes, Bytes)>,
    /// Streaming body channel — one chunk per `send(http.response.body)`.
    pub body_rx: mpsc::UnboundedReceiver<Bytes>,
}

// ── Wakeup ───────────────────────────────────────────────────────────────

/// Cross-platform wakeup signal for the asyncio thread.
///
/// Unix: socket fd pair — `signal()` writes 1 byte, asyncio wakes via
/// `loop.add_reader(fd)`. No GIL needed.
///
/// Under burst load, multiple tokio tasks may call `signal()` concurrently.
/// An [`AtomicBool`] flag coalesces redundant writes: only the first
/// `signal()` after a `drain()` actually writes to the pipe. This
/// eliminates the `Mutex` contention that serialized all signalers.
pub struct Wakeup {
    reader: std::os::unix::net::UnixStream,
    writer: std::os::unix::net::UnixStream,
    /// Coalescing flag — `true` means a wakeup byte is already in the pipe.
    pending: AtomicBool,
}

crate::opaque_debug!(Wakeup);

impl Wakeup {
    /// Create a new wakeup pipe pair.
    ///
    /// Both ends are set to non-blocking so neither `signal()` nor the
    /// asyncio `on_readable` callback can block.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the Unix socket pair cannot be created.
    pub fn new() -> io::Result<Self> {
        let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self {
            reader,
            writer,
            pending: AtomicBool::new(false),
        })
    }

    /// Signal the asyncio thread by writing 1 byte to the pipe.
    ///
    /// Uses CAS to coalesce: only the thread that flips `false→true`
    /// writes the byte. All others skip — a wakeup is already pending.
    /// POSIX guarantees atomicity for writes ≤ `PIPE_BUF`, so one byte
    /// is safe without a mutex.
    pub fn signal(&self) {
        if self
            .pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let _ = io::Write::write(&mut &self.writer, &[1u8]);
        }
    }

    /// Clear the pending flag after the asyncio thread drains the pipe.
    ///
    /// Called from [`crate::asgi::queue::RequestQueue::try_recv`] when
    /// the crossbeam queue is empty, allowing the next `signal()` to
    /// write a fresh wakeup byte.
    pub fn drain(&self) {
        self.pending.store(false, Ordering::Release);
    }

    /// Raw file descriptor for the reader end.
    ///
    /// Passed to `loop.add_reader(fd, callback)` during asyncio init.
    pub fn reader_fd(&self) -> std::os::unix::io::RawFd {
        std::os::unix::io::AsRawFd::as_raw_fd(&self.reader)
    }
}

// ── InboundChannel ───────────────────────────────────────────────────────

/// Thread 1 → Thread 2 request channel.
///
/// Unbounded crossbeam channel — backpressure is handled by the HTTP
/// semaphore in `ApxService`, not by the channel itself.
#[derive(Debug)]
pub struct InboundChannel {
    tx: crossbeam_channel::Sender<RequestSlot>,
    rx: crossbeam_channel::Receiver<RequestSlot>,
}

impl InboundChannel {
    /// Create a new unbounded inbound channel.
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self { tx, rx }
    }

    /// Sender half — cloned into each tokio task on Thread 1.
    pub fn sender(&self) -> &crossbeam_channel::Sender<RequestSlot> {
        &self.tx
    }

    /// Receiver half — used by `RequestQueue` on Thread 2.
    pub fn receiver(&self) -> &crossbeam_channel::Receiver<RequestSlot> {
        &self.rx
    }
}

// ── DispatchPipeline ─────────────────────────────────────────────────────

/// Bundles the inbound channel + wakeup. Created once per worker.
///
/// Responses flow directly from `SlotSend` to Thread 1 via tokio oneshot —
/// no outbound channel or relay thread needed.
#[derive(Debug)]
pub struct DispatchPipeline {
    /// Thread 1 → Thread 2 request channel.
    pub inbound: InboundChannel,
    /// Wakeup signal for the asyncio thread.
    pub wakeup: Arc<Wakeup>,
}

impl DispatchPipeline {
    /// Create a new dispatch pipeline with Unix pipe wakeup.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the wakeup pipe cannot be created.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inbound: InboundChannel::new(),
            wakeup: Arc::new(Wakeup::new()?),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;

    #[test]
    fn wakeup_signal_roundtrip() {
        let wakeup = Wakeup::new().unwrap();
        wakeup.signal();
        let mut buf = [0u8; 16];
        let n = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap();
        assert!(n > 0);
    }

    #[test]
    fn wakeup_coalescing_skips_redundant_writes() {
        let wakeup = Wakeup::new().unwrap();
        wakeup.signal();
        wakeup.signal();
        wakeup.signal();

        let mut buf = [0u8; 16];
        let n = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap();
        assert_eq!(n, 1, "coalesced signals should produce exactly 1 byte");

        let err = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn wakeup_drain_resets_flag() {
        let wakeup = Wakeup::new().unwrap();

        wakeup.signal();
        let mut buf = [0u8; 16];
        let _ = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap();

        // Before drain: second signal is suppressed (flag still true).
        wakeup.signal();
        let err = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        // After drain: flag cleared, next signal writes again.
        wakeup.drain();
        wakeup.signal();
        let n = io::Read::read(&mut &wakeup.reader, &mut buf).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn inbound_channel_send_recv() {
        let ch = InboundChannel::new();
        let (response_tx, _response_rx) = oneshot::channel();
        let slot = RequestSlot {
            method: http::Method::GET,
            path: "/test".to_owned(),
            raw_path: Bytes::from_static(b"/test"),
            query_string: Bytes::new(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            protocol: ProtocolVersion::Http11,
            client_addr: None,
            server_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            trace_context: None,
            created_at: std::time::Instant::now(),
            response_tx,
        };
        ch.sender().send(slot).unwrap();
        let received = ch.receiver().try_recv().unwrap();
        assert_eq!(received.path, "/test");
    }

    #[test]
    fn dispatch_pipeline_creates_successfully() {
        let pipeline = DispatchPipeline::new().unwrap();
        assert!(format!("{pipeline:?}").contains("DispatchPipeline"));
    }

    #[test]
    fn wakeup_reader_fd_is_valid() {
        let wakeup = Wakeup::new().unwrap();
        let fd = wakeup.reader_fd();
        assert!(fd >= 0);
    }

    #[test]
    fn request_slot_debug() {
        let (response_tx, _) = oneshot::channel();
        let slot = RequestSlot {
            method: http::Method::POST,
            path: "/api".to_owned(),
            raw_path: Bytes::from_static(b"/api"),
            query_string: Bytes::from_static(b"q=1"),
            headers: HeaderMap::new(),
            body: Bytes::from_static(b"{}"),
            protocol: ProtocolVersion::Http11,
            client_addr: Some(SocketAddr::from(([10, 0, 0, 1], 5000))),
            server_addr: SocketAddr::from(([0, 0, 0, 0], 8000)),
            trace_context: None,
            created_at: std::time::Instant::now(),
            response_tx,
        };
        let dbg = format!("{slot:?}");
        assert!(dbg.contains("RequestSlot"));
    }
}
