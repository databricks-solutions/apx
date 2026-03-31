//! IPC protocol messages between supervisor and workers.
//!
//! All messages are serialized as msgpack and framed with a 4-byte big-endian
//! length prefix. Python never touches these — they are Rust-internal.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Default event loop policy — `"uvloop"` for optimal performance.
fn default_loop_policy() -> String {
    "uvloop".to_owned()
}

// ── AppModule ───────────────────────────────────────────────────────────

/// Validation errors for Python dotted paths ([`AppModule`]).
#[derive(Debug, thiserror::Error)]
pub enum DottedPathError {
    /// The path was empty.
    #[error("{context} must not be empty")]
    Empty {
        /// What kind of path failed validation.
        context: &'static str,
    },
    /// A segment between dots was empty (e.g. `"foo..bar"`).
    #[error("{context} has empty segment: {value}")]
    EmptySegment {
        /// What kind of path failed validation.
        context: &'static str,
        /// The original input.
        value: String,
    },
    /// A segment is not a valid Python identifier.
    #[error("invalid {context} segment: {segment}")]
    InvalidSegment {
        /// What kind of path failed validation.
        context: &'static str,
        /// The invalid segment.
        segment: String,
    },
}

/// Validate a single segment of a Python dotted path.
fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with(|c: char| c.is_ascii_digit())
        && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Validate a Python dotted path (e.g. `"backend.app.handler"`).
fn validate_dotted_path(path: &str, context: &'static str) -> Result<(), DottedPathError> {
    if path.is_empty() {
        return Err(DottedPathError::Empty { context });
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(DottedPathError::EmptySegment {
                context,
                value: path.to_owned(),
            });
        }
        if !is_valid_segment(segment) {
            return Err(DottedPathError::InvalidSegment {
                context,
                segment: segment.to_owned(),
            });
        }
    }
    Ok(())
}

/// Python module path: `"backend.app"`, `"mypackage.api"`.
///
/// Must be a valid Python dotted path. Format validation only — runtime
/// validation (module importable, contains App instance) happens during
/// worker discovery.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppModule(String);

impl AppModule {
    /// Error context for validation messages.
    const CONTEXT: &str = "app module";

    /// Create a new module path, validating all segments.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is empty or any segment is invalid.
    pub fn new(module: impl Into<String>) -> Result<Self, DottedPathError> {
        let module = module.into();
        validate_dotted_path(&module, Self::CONTEXT)?;
        Ok(Self(module))
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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

    /// Worker → Supervisor: telemetry config read from Python for supervisor relay.
    TelemetryConfig(TelemetryRelay),

    /// Worker → Supervisor: startup failed with error details.
    StartupFailed {
        /// Human-readable error message from the failed startup step.
        error: String,
    },

    /// Supervisor → Worker: stop accepting, finish in-flight requests.
    Drain,

    /// Worker → Supervisor: drain complete, about to exit.
    Drained,
}

/// Telemetry config relayed from a worker to the supervisor.
///
/// The first worker (worker 0) sends this after loading the Python app
/// and reading the telemetry configuration. The supervisor uses it to
/// start system-global and supervisor process metrics with user toggles
/// instead of Rust defaults.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TelemetryRelay {
    /// System-global metrics configuration (supervisor only).
    pub system: crate::telemetry::config::SystemConfig,
    /// Per-process metrics configuration (supervisor's own process).
    pub process: crate::telemetry::config::ProcessConfig,
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
    /// Maximum concurrent requests per worker (`None` → framework default).
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    /// One-time nonce — verified against `APX_WORKER_NONCE` env var.
    pub nonce: Nonce,
    /// Event loop policy: `"asyncio"` (default stdlib) or `"uvloop"`.
    /// Workers install the corresponding policy before creating the event loop.
    #[serde(default = "default_loop_policy")]
    pub loop_policy: String,
    /// If true, the worker sends a `TelemetryConfig` message after app load.
    #[serde(default)]
    pub relay_telemetry: bool,
    /// Maximum seconds the worker should spend draining in-flight connections
    /// before giving up and exiting. Must be less than the supervisor's drain
    /// timeout so the worker can send `Drained` before being killed.
    #[serde(default = "default_drain_timeout_secs")]
    pub drain_timeout_secs: u64,
    /// Enable dev-mode error visibility (tracebacks in 500 response bodies).
    #[serde(default)]
    pub dev_mode: bool,
}

fn default_drain_timeout_secs() -> u64 {
    5
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
            max_concurrent: None,
            nonce: Nonce::generate(),
            loop_policy: "uvloop".to_owned(),
            relay_telemetry: false,
            drain_timeout_secs: 5,
            dev_mode: false,
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
            other => unreachable!("expected Bootstrap, got {other:?}"),
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
    fn ipc_message_drain_roundtrip() {
        let msg = IpcMessage::Drain;
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("Drain should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("Drain should be deserializable: {e}"));
        assert!(matches!(decoded, IpcMessage::Drain));
    }

    #[test]
    fn ipc_message_drained_roundtrip() {
        let msg = IpcMessage::Drained;
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("Drained should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("Drained should be deserializable: {e}"));
        assert!(matches!(decoded, IpcMessage::Drained));
    }

    #[test]
    fn ipc_message_startup_failed_roundtrip() {
        let msg = IpcMessage::StartupFailed {
            error: "app load failed: no attribute 'app' in module 'main'".to_owned(),
        };
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("StartupFailed should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("StartupFailed should be deserializable: {e}"));
        match decoded {
            IpcMessage::StartupFailed { error } => {
                assert!(error.contains("no attribute"));
            }
            other => unreachable!("expected StartupFailed, got {other:?}"),
        }
    }

    #[test]
    fn ipc_message_startup_failed_multiline_traceback_roundtrip() {
        let traceback = "\
Traceback (most recent call last):
  File \"/app/router.py\", line 11, in <module>
    x = undefined_var
NameError: name 'undefined_var' is not defined
";
        let msg = IpcMessage::StartupFailed {
            error: traceback.to_owned(),
        };
        let encoded = rmp_serde::to_vec(&msg)
            .unwrap_or_else(|e| unreachable!("StartupFailed should be serializable: {e}"));
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded)
            .unwrap_or_else(|e| unreachable!("StartupFailed should be deserializable: {e}"));
        match decoded {
            IpcMessage::StartupFailed { error } => {
                assert!(error.contains("Traceback"));
                assert!(error.contains("NameError"));
                assert!(error.contains("router.py"));
            }
            other => unreachable!("expected StartupFailed, got {other:?}"),
        }
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
    fn worker_bootstrap_serde_loop_policy() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app").unwrap(),
            request_timeout_secs: 30,
            max_concurrent: None,
            nonce: Nonce::from_string("abc123".to_owned()),
            loop_policy: "uvloop".to_owned(),
            relay_telemetry: false,
            drain_timeout_secs: 5,
            dev_mode: false,
        };
        let encoded = rmp_serde::to_vec(&bootstrap).unwrap();
        let decoded: WorkerBootstrap = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded.loop_policy, "uvloop");
    }

    #[test]
    fn worker_bootstrap_serde_default_loop_policy() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app").unwrap(),
            request_timeout_secs: 30,
            max_concurrent: None,
            nonce: Nonce::from_string("abc123".to_owned()),
            loop_policy: default_loop_policy(),
            relay_telemetry: false,
            drain_timeout_secs: 5,
            dev_mode: false,
        };
        assert_eq!(bootstrap.loop_policy, "uvloop");
    }

    #[test]
    fn worker_bootstrap_relay_telemetry_roundtrip() {
        let bootstrap = WorkerBootstrap {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            app_module: AppModule::new("backend.app").unwrap(),
            request_timeout_secs: 30,
            max_concurrent: None,
            nonce: Nonce::from_string("abc123".to_owned()),
            loop_policy: "uvloop".to_owned(),
            relay_telemetry: true,
            drain_timeout_secs: 5,
            dev_mode: false,
        };
        let encoded = rmp_serde::to_vec(&bootstrap).unwrap();
        let decoded: WorkerBootstrap = rmp_serde::from_slice(&encoded).unwrap();
        assert!(decoded.relay_telemetry);
    }

    #[test]
    fn ipc_message_telemetry_config_roundtrip() {
        use crate::telemetry::config::{
            ProcessConfig, ProcessMetricToggles, SystemConfig, SystemGlobalToggles,
        };

        let relay = TelemetryRelay {
            system: SystemConfig {
                enabled: true,
                metrics: SystemGlobalToggles {
                    system_cpu: true,
                    system_memory: false,
                    system_paging: true,
                    system_disk_io: false,
                    system_network_io: true,
                },
            },
            process: ProcessConfig {
                enabled: false,
                metrics: ProcessMetricToggles {
                    process_cpu: false,
                    process_memory: true,
                    process_threads: false,
                },
            },
        };
        let msg = IpcMessage::TelemetryConfig(relay);
        let encoded = rmp_serde::to_vec(&msg).unwrap();
        let decoded: IpcMessage = rmp_serde::from_slice(&encoded).unwrap();
        match decoded {
            IpcMessage::TelemetryConfig(r) => {
                assert!(r.system.enabled);
                assert!(r.system.metrics.system_cpu);
                assert!(!r.system.metrics.system_memory);
                assert!(!r.process.enabled);
                assert!(r.process.metrics.process_memory);
            }
            other => unreachable!("expected TelemetryConfig, got {other:?}"),
        }
    }
}
