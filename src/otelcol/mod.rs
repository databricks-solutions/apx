//! OpenTelemetry Collector management for centralized log collection.
//!
//! This module manages a global otelcol instance that runs at port 11111.
//! All APX dev servers send logs to this collector via OTLP HTTP.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tracing::{debug, info};

use crate::interop::otelcol_binary_path;

/// Default port for otelcol OTLP HTTP receiver
pub const OTELCOL_PORT: u16 = 11111;

/// Directory for otelcol config and logs
const OTELCOL_DIR: &str = ".apx/logs";

/// Config filename
const CONFIG_FILENAME: &str = "otelcol.yaml";

/// Lock filename
const LOCK_FILENAME: &str = "otelcol.lock";

/// Embedded otelcol configuration template.
/// The `{logs_dir}` placeholder is replaced with the actual logs directory.
const OTELCOL_CONFIG_TEMPLATE: &str = r#"receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:11111

processors:
  batch:
    timeout: 100ms
    send_batch_size: 128

exporters:
  file/logs:
    path: {logs_dir}/logs.json
    rotation:
      max_megabytes: 50
      max_backups: 5

service:
  pipelines:
    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: [file/logs]
  telemetry:
    logs:
      level: warn
"#;

/// Lock file contents for tracking otelcol process
#[derive(Debug)]
struct OtelcolLock {
    pid: u32,
    config_hash: String,
}

impl OtelcolLock {
    fn parse(contents: &str) -> Option<Self> {
        let mut pid = None;
        let mut config_hash = None;

        for line in contents.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("pid=") {
                pid = val.parse().ok();
            } else if let Some(val) = line.strip_prefix("config_hash=") {
                config_hash = Some(val.to_string());
            }
        }

        Some(Self {
            pid: pid?,
            config_hash: config_hash?,
        })
    }

    fn to_string(&self) -> String {
        format!("pid={}\nconfig_hash={}\n", self.pid, self.config_hash)
    }
}

/// Get the otelcol directory path (~/.apx/logs)
fn otelcol_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    Ok(home.join(OTELCOL_DIR))
}

/// Get the config file path
fn config_path() -> Result<PathBuf, String> {
    Ok(otelcol_dir()?.join(CONFIG_FILENAME))
}

/// Get the lock file path
fn lock_path() -> Result<PathBuf, String> {
    Ok(otelcol_dir()?.join(LOCK_FILENAME))
}

/// Generate the otelcol configuration with the logs directory
fn generate_config(logs_dir: &Path) -> String {
    OTELCOL_CONFIG_TEMPLATE.replace("{logs_dir}", &logs_dir.display().to_string())
}

/// Compute SHA256 hash of config content (first 12 chars)
fn config_hash(config: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..6]) // 12 hex chars
}

/// Check if otelcol is accepting connections at the given port
fn is_otelcol_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Check if a process with the given PID is still running
fn is_process_running(pid: u32) -> bool {
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.process(Pid::from_u32(pid)).is_some()
}

/// Read the lock file if it exists
fn read_lock() -> Result<Option<OtelcolLock>, String> {
    let path = lock_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read otelcol lock file: {}", e))?;

    Ok(OtelcolLock::parse(&contents))
}

/// Write the lock file
fn write_lock(lock: &OtelcolLock) -> Result<(), String> {
    let path = lock_path()?;
    fs::write(&path, lock.to_string())
        .map_err(|e| format!("Failed to write otelcol lock file: {}", e))
}

/// Remove the lock file
fn remove_lock() -> Result<(), String> {
    let path = lock_path()?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove otelcol lock file: {}", e))?;
    }
    Ok(())
}

/// Start the otelcol process
fn start_otelcol(config_path: &Path) -> Result<Child, String> {
    let binary = otelcol_binary_path()?;

    if !binary.exists() {
        return Err(format!(
            "otelcol binary not found at {}",
            binary.display()
        ));
    }

    debug!(
        "Starting otelcol: {} --config {}",
        binary.display(),
        config_path.display()
    );

    let child = Command::new(&binary)
        .arg("--config")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start otelcol: {}", e))?;

    Ok(child)
}

/// Wait for otelcol to start accepting connections
fn wait_for_otelcol_ready(timeout_ms: u64) -> Result<(), String> {
    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    while start.elapsed() < timeout {
        if is_otelcol_listening(OTELCOL_PORT) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "otelcol did not start within {}ms",
        timeout_ms
    ))
}

/// Ensure otelcol is running with the correct configuration.
///
/// This function:
/// 1. Checks if otelcol is already running with correct config
/// 2. If not, starts a new otelcol process
/// 3. Waits for it to become ready
///
/// Returns Ok(()) if otelcol is running and ready.
pub fn ensure_otelcol_running() -> Result<(), String> {
    let logs_dir = otelcol_dir()?;
    fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("Failed to create logs directory: {}", e))?;

    let config = generate_config(&logs_dir);
    let expected_hash = config_hash(&config);

    // Check existing lock
    if let Some(lock) = read_lock()? {
        // Check if config matches and process is still running
        if lock.config_hash == expected_hash && is_process_running(lock.pid) {
            // Also verify it's actually listening
            if is_otelcol_listening(OTELCOL_PORT) {
                debug!(
                    "otelcol already running (pid={}, hash={})",
                    lock.pid, lock.config_hash
                );
                return Ok(());
            }
        }

        // Process is gone or config changed, clean up
        debug!("Stale otelcol lock found, cleaning up");
        remove_lock()?;
    }

    // Check if something else is using the port
    if is_otelcol_listening(OTELCOL_PORT) {
        // Try to use it anyway - might be a previous otelcol without lock
        return Ok(());
    }

    // Write config file
    let cfg_path = config_path()?;
    fs::write(&cfg_path, &config)
        .map_err(|e| format!("Failed to write otelcol config: {}", e))?;
    debug!("Wrote otelcol config to {}", cfg_path.display());

    // Start otelcol
    info!("Starting otelcol at port {}", OTELCOL_PORT);
    let mut child = start_otelcol(&cfg_path)?;

    // Get PID before we potentially lose the handle
    let pid = child.id();

    // Spawn a thread to read stderr for debugging
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                debug!("[otelcol] {}", line);
            }
        });
    }

    // Wait for otelcol to start
    wait_for_otelcol_ready(5000)?;

    // Write lock file
    let lock = OtelcolLock {
        pid,
        config_hash: expected_hash,
    };
    write_lock(&lock)?;

    info!("otelcol started successfully (pid={})", pid);
    Ok(())
}

/// Get the logs directory for a specific app.
/// Uses a hash of the app path to create a unique subdirectory.
#[allow(dead_code)]
pub fn app_logs_dir(app_path: &Path) -> Result<PathBuf, String> {
    let canonical = app_path
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize app path: {}", e))?;

    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    let hash = hex::encode(&result[..6]); // 12 hex chars

    let logs_dir = otelcol_dir()?.join(&hash);
    fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("Failed to create app logs directory: {}", e))?;

    Ok(logs_dir)
}

/// Get the OTEL endpoint URL
#[allow(dead_code)]
pub fn otel_endpoint() -> String {
    format!("http://127.0.0.1:{}", OTELCOL_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_generation() {
        let config = generate_config(Path::new("/tmp/logs"));
        assert!(config.contains("endpoint: 0.0.0.0:11111"));
        assert!(config.contains("path: /tmp/logs/logs.json"));
    }

    #[test]
    fn test_config_hash() {
        let config = "test config";
        let hash = config_hash(config);
        assert_eq!(hash.len(), 12);
    }

    #[test]
    fn test_lock_parse() {
        let contents = "pid=1234\nconfig_hash=abc123def456\n";
        let lock = OtelcolLock::parse(contents).unwrap();
        assert_eq!(lock.pid, 1234);
        assert_eq!(lock.config_hash, "abc123def456");
    }
}
