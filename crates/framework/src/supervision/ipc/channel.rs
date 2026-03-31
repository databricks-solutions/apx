//! Bidirectional IPC channel with length-prefixed msgpack framing.
//!
//! Each message is `[4 bytes big-endian u32 length][msgpack payload]`.
//! Pure binary — no text delimiters, no scanning.

use super::protocol::{IpcError, IpcMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Max IPC message size (1 MiB). Prevents a malformed length prefix
/// from allocating unbounded memory.
pub const MAX_IPC_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Bidirectional IPC channel between supervisor and worker.
///
/// UDS on Unix. Python never touches this.
pub struct WorkerChannel {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

crate::opaque_debug!(WorkerChannel);

impl WorkerChannel {
    /// Create a channel from a connected UDS split into read/write halves.
    fn from_split(read: OwnedReadHalf, write: OwnedWriteHalf) -> Self {
        Self {
            reader: BufReader::new(read),
            writer: write,
        }
    }

    /// Split into independent reader and writer halves.
    ///
    /// Useful when the worker needs to read drain commands from a spawned
    /// task while the main flow still holds the writer to send `Drained`.
    pub fn split(self) -> (IpcReader, IpcWriter) {
        (
            IpcReader {
                reader: self.reader,
            },
            IpcWriter {
                writer: self.writer,
            },
        )
    }

    /// Send a length-prefixed msgpack message.
    ///
    /// # Errors
    ///
    /// Returns an error on serialization failure or IO error.
    pub async fn send(&mut self, msg: &IpcMessage) -> Result<(), IpcError> {
        IpcWriter::send_impl(&mut self.writer, msg).await
    }

    /// Receive a length-prefixed msgpack message.
    ///
    /// # Errors
    ///
    /// Returns an error on IO error, deserialization failure, or if the
    /// message exceeds [`MAX_IPC_MESSAGE_SIZE`].
    pub async fn recv(&mut self) -> Result<IpcMessage, IpcError> {
        IpcReader::recv_impl(&mut self.reader).await
    }
}

/// Read half of an IPC channel.
pub struct IpcReader {
    reader: BufReader<OwnedReadHalf>,
}

crate::opaque_debug!(IpcReader);

impl IpcReader {
    /// Receive a length-prefixed msgpack message.
    pub async fn recv(&mut self) -> Result<IpcMessage, IpcError> {
        Self::recv_impl(&mut self.reader).await
    }

    async fn recv_impl(reader: &mut BufReader<OwnedReadHalf>) -> Result<IpcMessage, IpcError> {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);
        if len > MAX_IPC_MESSAGE_SIZE {
            return Err(IpcError::MessageTooLarge(len as usize));
        }
        let mut payload = vec![0u8; len as usize];
        reader.read_exact(&mut payload).await?;
        Ok(rmp_serde::from_slice(&payload)?)
    }
}

/// Write half of an IPC channel.
pub struct IpcWriter {
    writer: OwnedWriteHalf,
}

crate::opaque_debug!(IpcWriter);

impl IpcWriter {
    /// Send a length-prefixed msgpack message.
    pub async fn send(&mut self, msg: &IpcMessage) -> Result<(), IpcError> {
        Self::send_impl(&mut self.writer, msg).await
    }

    async fn send_impl(writer: &mut OwnedWriteHalf, msg: &IpcMessage) -> Result<(), IpcError> {
        let payload = rmp_serde::to_vec(msg)?;
        let len =
            u32::try_from(payload.len()).map_err(|_| IpcError::MessageTooLarge(payload.len()))?;
        writer.write_all(&len.to_be_bytes()).await?;
        writer.write_all(&payload).await?;
        writer.flush().await?;
        Ok(())
    }
}

// ── Platform-specific connection ────────────────────────────────────────

/// Connect to a supervisor's UDS (worker side).
///
/// # Errors
///
/// Returns an error if the connection fails.
#[cfg(unix)]
pub async fn connect(path: &str) -> Result<WorkerChannel, IpcError> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (read, write) = stream.into_split();
    Ok(WorkerChannel::from_split(read, write))
}

/// Create a UDS listener for workers to connect to (supervisor side).
///
/// Creates the parent directory with mode 0700 (owner-only) to prevent
/// unauthorized access to the IPC socket.
///
/// # Errors
///
/// Returns an error if directory creation or socket binding fails.
#[cfg(unix)]
pub fn listen(path: &str) -> Result<tokio::net::UnixListener, IpcError> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(parent)?;
    }
    Ok(tokio::net::UnixListener::bind(path)?)
}

/// Accept a worker connection and return a channel (supervisor side).
///
/// # Errors
///
/// Returns an error if accept fails.
#[cfg(unix)]
pub async fn accept(listener: &tokio::net::UnixListener) -> Result<WorkerChannel, IpcError> {
    let (stream, _addr) = listener.accept().await?;
    let (read, write) = stream.into_split();
    Ok(WorkerChannel::from_split(read, write))
}

#[cfg(windows)]
compile_error!("Windows IPC (Named Pipes) is not yet implemented. See extensions.md.");

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::supervision::ipc::protocol::AppModule;
    use crate::supervision::ipc::protocol::{Nonce, WorkerBootstrap};

    #[tokio::test]
    async fn ipc_bootstrap_roundtrip_over_uds() {
        let dir =
            tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir should succeed: {e}"));
        let sock_path = dir.path().join("test.sock");
        let sock_str = sock_path
            .to_str()
            .unwrap_or_else(|| unreachable!("tempdir path should be UTF-8"));

        let listener =
            listen(sock_str).unwrap_or_else(|e| unreachable!("listen should succeed: {e}"));

        // Simulate supervisor + worker communication.
        let nonce = Nonce::generate();
        let bootstrap = WorkerBootstrap {
            host: "127.0.0.1".to_owned(),
            port: 9000,
            app_module: AppModule::new("backend.app")
                .unwrap_or_else(|e| unreachable!("valid module: {e}")),
            request_timeout_secs: 30,
            max_concurrent: None,
            nonce,
            loop_policy: "uvloop".to_owned(),
            relay_telemetry: false,
            drain_timeout_secs: 5,
            dev_mode: false,
        };

        let supervisor_handle = tokio::spawn(async move {
            let mut ch = accept(&listener)
                .await
                .unwrap_or_else(|e| unreachable!("accept should succeed: {e}"));
            ch.send(&IpcMessage::Bootstrap(bootstrap))
                .await
                .unwrap_or_else(|e| unreachable!("send should succeed: {e}"));
            let msg = ch
                .recv()
                .await
                .unwrap_or_else(|e| unreachable!("recv should succeed: {e}"));
            assert!(matches!(msg, IpcMessage::Ready));
        });

        let mut worker_ch = connect(sock_str)
            .await
            .unwrap_or_else(|e| unreachable!("connect should succeed: {e}"));
        let msg = worker_ch
            .recv()
            .await
            .unwrap_or_else(|e| unreachable!("recv should succeed: {e}"));
        match msg {
            IpcMessage::Bootstrap(b) => {
                assert_eq!(b.host, "127.0.0.1");
                assert_eq!(b.port, 9000);
            }
            other => unreachable!("expected Bootstrap, got {other:?}"),
        }
        worker_ch
            .send(&IpcMessage::Ready)
            .await
            .unwrap_or_else(|e| unreachable!("send Ready should succeed: {e}"));

        supervisor_handle
            .await
            .unwrap_or_else(|e| unreachable!("supervisor task should complete: {e}"));
    }

    #[tokio::test]
    async fn ipc_channel_eof_on_worker_exit() {
        let dir =
            tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir should succeed: {e}"));
        let sock_path = dir.path().join("eof.sock");
        let sock_str = sock_path
            .to_str()
            .unwrap_or_else(|| unreachable!("path should be UTF-8"));

        let listener =
            listen(sock_str).unwrap_or_else(|e| unreachable!("listen should succeed: {e}"));

        let supervisor_handle = tokio::spawn(async move {
            let mut ch = accept(&listener)
                .await
                .unwrap_or_else(|e| unreachable!("accept should succeed: {e}"));
            // Worker will drop its channel — recv should return an error.
            let result = ch.recv().await;
            assert!(result.is_err(), "expected EOF error after worker exit");
        });

        // Connect and immediately drop (simulate worker exit).
        let worker_ch = connect(sock_str)
            .await
            .unwrap_or_else(|e| unreachable!("connect should succeed: {e}"));
        drop(worker_ch);

        supervisor_handle
            .await
            .unwrap_or_else(|e| unreachable!("supervisor task should complete: {e}"));
    }

    #[tokio::test]
    async fn ipc_channel_recv_message_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("large.sock");
        let sock_str = sock_path.to_str().unwrap();

        let listener = listen(sock_str).unwrap();

        let supervisor_handle = tokio::spawn(async move {
            let mut ch = accept(&listener).await.unwrap();
            let result = ch.recv().await;
            assert!(
                matches!(result, Err(IpcError::MessageTooLarge(_))),
                "expected MessageTooLarge, got {result:?}"
            );
        });

        let stream = tokio::net::UnixStream::connect(sock_str).await.unwrap();
        let (_, mut write) = stream.into_split();
        // Write length header indicating a message larger than MAX_IPC_MESSAGE_SIZE
        let huge_len = MAX_IPC_MESSAGE_SIZE + 1;
        AsyncWriteExt::write_all(&mut write, &huge_len.to_be_bytes())
            .await
            .unwrap();

        supervisor_handle.await.unwrap();
    }

    #[tokio::test]
    async fn ipc_channel_recv_malformed_payload() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("malformed.sock");
        let sock_str = sock_path.to_str().unwrap();

        let listener = listen(sock_str).unwrap();

        let supervisor_handle = tokio::spawn(async move {
            let mut ch = accept(&listener).await.unwrap();
            let result = ch.recv().await;
            assert!(
                matches!(result, Err(IpcError::Decode(_))),
                "expected Decode error, got {result:?}"
            );
        });

        let stream = tokio::net::UnixStream::connect(sock_str).await.unwrap();
        let (_, mut write) = stream.into_split();
        // Write valid length (10) then garbage bytes
        let len: u32 = 10;
        AsyncWriteExt::write_all(&mut write, &len.to_be_bytes())
            .await
            .unwrap();
        AsyncWriteExt::write_all(&mut write, &[0xFF; 10])
            .await
            .unwrap();

        supervisor_handle.await.unwrap();
    }

    #[test]
    fn max_ipc_message_size_is_one_mib() {
        assert_eq!(MAX_IPC_MESSAGE_SIZE, 1024 * 1024);
    }

    #[tokio::test]
    async fn worker_channel_debug() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("debug.sock");
        let sock_str = sock_path.to_str().unwrap();

        let listener = listen(sock_str).unwrap();

        let supervisor_handle = tokio::spawn(async move {
            let ch = accept(&listener).await.unwrap();
            let dbg = format!("{ch:?}");
            assert!(dbg.contains("WorkerChannel"));
        });

        let _worker = connect(sock_str).await.unwrap();
        supervisor_handle.await.unwrap();
    }
}
