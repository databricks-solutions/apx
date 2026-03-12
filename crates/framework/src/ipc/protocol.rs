//! IPC protocol messages between supervisor and workers.
//!
//! All messages are serialized as msgpack and framed with a 4-byte big-endian
//! length prefix. Python never touches these — they are Rust-internal.

use crate::route::AppModule;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

// ── Nonce ───────────────────────────────────────────────────────────────

/// One-time nonce for worker bootstrap verification.
///
/// Uses constant-time comparison to prevent timing attacks. Debug output
/// is redacted to prevent leaking nonce values in logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct Nonce(String);

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Nonce").field(&"[REDACTED]").finish()
    }
}

impl Nonce {
    /// Generate a cryptographically random 32-byte hex nonce.
    pub fn generate() -> Self {
        use rand::Rng;
        let bytes: [u8; 32] = rand::thread_rng().r#gen();
        let mut buf = String::with_capacity(64);
        for byte in &bytes {
            use std::fmt::Write;
            let _ = write!(buf, "{byte:02x}");
        }
        Self(buf)
    }

    /// Create a nonce from a string (e.g. from an environment variable).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Return the inner string for env var propagation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison — prevents timing side-channels.
    pub fn verify(&self, other: &Self) -> bool {
        let a = self.0.as_bytes();
        let b = other.0.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        a.iter()
            .zip(b.iter())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
    }
}

// ── IPC error ───────────────────────────────────────────────────────────

/// Errors during IPC communication.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// IO error on the underlying transport.
    #[error("ipc io: {0}")]
    Io(#[from] std::io::Error),

    /// Msgpack serialization failed.
    #[error("ipc encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    /// Msgpack deserialization failed.
    #[error("ipc decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),

    /// Message exceeds [`MAX_IPC_MESSAGE_SIZE`](super::channel::MAX_IPC_MESSAGE_SIZE).
    #[error("ipc message too large: {0} bytes")]
    MessageTooLarge(usize),
}

// ── Protocol messages ───────────────────────────────────────────────────

/// All messages that flow over the supervisor ↔ worker channel.
///
/// Tagged enum with serde — serialized as msgpack over the wire.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcMessage {
    /// Supervisor → Worker: initial configuration (first message after connect).
    Bootstrap(WorkerBootstrap),

    /// Worker → Supervisor: worker is ready to accept HTTP traffic.
    Ready,
}

/// Bootstrap config sent to the worker over the IPC channel.
///
/// The worker reads this, validates the nonce against `APX_WORKER_NONCE`,
/// then binds its TCP listener independently.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerBootstrap {
    /// Host to bind to.
    pub host: String,
    /// Port to bind to.
    pub port: u16,
    /// Python module path (e.g., `"backend.app"`).
    pub app_module: AppModule,
    /// Request timeout in seconds (converted to `Duration` at the worker boundary).
    pub request_timeout_secs: u64,
    /// One-time nonce — verified against `APX_WORKER_NONCE` env var.
    pub nonce: Nonce,
    /// Path to the pre-built `AppManifest` JSON file.
    /// `None` when using live-import mode (app module imported at worker startup).
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
}

// ── Bootstrap errors ────────────────────────────────────────────────────

/// Errors during worker bootstrap.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// Failed to connect to the IPC socket.
    #[error("failed to connect to IPC socket at '{path}': {source}")]
    Connect {
        /// Socket path.
        path: String,
        /// Underlying IO error.
        source: std::io::Error,
    },

    /// Failed to receive bootstrap message.
    #[error("failed to receive bootstrap message: {0}")]
    Receive(#[from] IpcError),

    /// First IPC message was not Bootstrap.
    #[error("first IPC message was not Bootstrap, got: {0}")]
    UnexpectedMessage(String),

    /// `APX_WORKER_NONCE` env var not set.
    #[error("APX_WORKER_NONCE env var not set — not spawned by supervisor?")]
    MissingNonce,

    /// Nonce mismatch between env var and IPC payload.
    #[error("nonce mismatch — rejecting bootstrap (possible rogue process)")]
    NonceMismatch,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn nonce_verify_equal() {
        let a = Nonce::generate();
        let b = Nonce(a.0.clone());
        assert!(a.verify(&b));
    }

    #[test]
    fn nonce_verify_unequal() {
        let a = Nonce::generate();
        let b = Nonce::generate();
        // Two random nonces should differ (probability of collision ≈ 0).
        assert!(!a.verify(&b));
    }

    #[test]
    fn nonce_verify_different_length() {
        let a = Nonce("short".to_owned());
        let b = Nonce("muchlongervalue".to_owned());
        assert!(!a.verify(&b));
    }

    #[test]
    fn nonce_debug_is_redacted() {
        let n = Nonce::generate();
        let debug = format!("{n:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains(&n.0));
    }

    #[test]
    fn ipc_message_roundtrip() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app")
                .unwrap_or_else(|e| unreachable!("hardcoded valid module: {e}")),
            request_timeout_secs: 30,
            nonce: Nonce::generate(),
            manifest_path: Some(PathBuf::from("/app/manifest.json")),
        };
        let msg = IpcMessage::Bootstrap(bootstrap);
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("IpcMessage should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("IpcMessage should be deserializable: {e}"));
        match decoded {
            IpcMessage::Bootstrap(b) => {
                assert_eq!(b.host, "0.0.0.0");
                assert_eq!(b.port, 8000);
            }
            IpcMessage::Ready => unreachable!("expected Bootstrap"),
        }
    }

    #[test]
    fn ipc_message_ready_roundtrip() {
        let msg = IpcMessage::Ready;
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("Ready should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("Ready should be deserializable: {e}"));
        assert!(matches!(decoded, IpcMessage::Ready));
    }

    #[test]
    fn nonce_generate_length_and_hex() {
        let n = Nonce::generate();
        assert_eq!(n.as_str().len(), 64);
        assert!(n.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonce_from_string_roundtrip() {
        let n = Nonce::from_string("deadbeef".to_owned());
        assert_eq!(n.as_str(), "deadbeef");
    }

    #[test]
    fn bootstrap_error_display_connect() {
        let err = BootstrapError::Connect {
            path: "/tmp/test.sock".to_owned(),
            source: std::io::Error::other("refused"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("/tmp/test.sock"));
        assert!(msg.contains("refused"));
    }

    #[test]
    fn bootstrap_error_display_missing_nonce() {
        let err = BootstrapError::MissingNonce;
        let msg = format!("{err}");
        assert!(msg.contains("APX_WORKER_NONCE"));
    }

    #[test]
    fn bootstrap_error_display_nonce_mismatch() {
        let err = BootstrapError::NonceMismatch;
        let msg = format!("{err}");
        assert!(msg.contains("mismatch"));
    }

    #[test]
    fn bootstrap_error_display_unexpected_message() {
        let err = BootstrapError::UnexpectedMessage("Ready".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("Ready"));
    }

    #[test]
    fn ipc_error_display_io() {
        let err = IpcError::Io(std::io::Error::other("broken pipe"));
        let msg = format!("{err}");
        assert!(msg.contains("broken pipe"));
    }

    #[test]
    fn ipc_error_display_message_too_large() {
        let err = IpcError::MessageTooLarge(2_000_000);
        let msg = format!("{err}");
        assert!(msg.contains("2000000"));
    }

    #[test]
    fn worker_bootstrap_serde_with_manifest_path() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app").unwrap(),
            request_timeout_secs: 30,
            nonce: Nonce::from_string("abc123".to_owned()),
            manifest_path: Some(PathBuf::from("/app/manifest.json")),
        };
        let encoded = rmp_serde::to_vec(&bootstrap).unwrap();
        let decoded: WorkerBootstrap = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded.manifest_path,
            Some(PathBuf::from("/app/manifest.json"))
        );
    }

    #[test]
    fn worker_bootstrap_serde_without_manifest_path() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app").unwrap(),
            request_timeout_secs: 30,
            nonce: Nonce::from_string("abc123".to_owned()),
            manifest_path: None,
        };
        let encoded = rmp_serde::to_vec(&bootstrap).unwrap();
        let decoded: WorkerBootstrap = rmp_serde::from_slice(&encoded).unwrap();
        assert!(decoded.manifest_path.is_none());
    }
}
