//! Embedded database lifecycle manager for the APX dev server.
//!
//! Encapsulates PGlite spawning, readiness polling, credential rotation,
//! and health monitoring. No PGlite-specific details leak beyond this module.
// Runs inside the dev server child process (spawned with Stdio::null()),
// never in the MCP server process — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{debug, warn};

use crate::dev::otel::forward_log_to_flux;
use crate::dev::token;
use crate::external::ExternalTool;
use crate::external::bun::Bun;
use apx_common::hosts::CLIENT_HOST;

/// Maximum number of readiness polls (30 * 100ms = 3 seconds).
const READINESS_POLL_LIMIT: usize = 30;

/// Interval between readiness polls.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Health monitor poll interval.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Health monitor timeout (stop checking after this duration).
const HEALTH_MONITOR_TIMEOUT_SECS: i64 = 60;

/// Default PGlite username for initial connection.
const DEFAULT_USER: &str = "postgres";

/// Default PGlite database name.
const DEFAULT_DB: &str = "postgres";

/// Self-contained embedded database lifecycle manager.
/// Encapsulates PGlite spawning, readiness polling, credential rotation,
/// and health monitoring. ProcessManager interacts only through this API.
pub(crate) struct EmbeddedDb {
    child: Arc<Mutex<Option<Child>>>,
    port: u16,
    password: String,
}

// `Child` does not implement `Debug`, so we provide a manual impl.
impl std::fmt::Debug for EmbeddedDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedDb")
            .field("port", &self.port)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[allow(dead_code)]
impl EmbeddedDb {
    /// Spawn PGlite via bun, wait for TCP readiness, rotate the default
    /// password, and start a background health monitor.
    pub async fn start(
        app_dir: &Path,
        host: &str,
        port: u16,
        app_slug: &str,
    ) -> Result<Self, String> {
        let bun = Bun::new().await?;
        let password = token::generate();

        let child = Self::spawn_pglite(&bun, app_dir, host, port, app_slug).await?;
        let child = Arc::new(Mutex::new(Some(child)));

        Self::wait_for_ready(port).await?;
        Self::rotate_password(port, &password).await?;
        debug!("Embedded database password rotated successfully");

        Self::spawn_health_monitor(Arc::clone(&child));

        Ok(Self {
            child,
            port,
            password,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    /// Access the child handle for parallel shutdown operations
    /// used by `ProcessManager::stop()`.
    pub fn child_handle(&self) -> &Arc<Mutex<Option<Child>>> {
        &self.child
    }

    /// Process-alive check (no HTTP probe — PGlite has no HTTP endpoint).
    pub async fn status(&self) -> &'static str {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => "stopped",
                Ok(None) => "healthy",
                Err(_) => "unknown",
            },
            None => "stopped",
        }
    }

    // -- private helpers --

    async fn spawn_pglite(
        bun: &Bun,
        app_dir: &Path,
        host: &str,
        port: u16,
        app_slug: &str,
    ) -> Result<Child, String> {
        let mut cmd = Command::new(bun.binary_path());
        cmd.args([
            "x",
            "@electric-sql/pglite-socket",
            "--db=memory://",
            &format!("--host={host}"),
            "--debug=0",
            &format!("--port={port}"),
        ])
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|err| format!("Failed to start embedded database: {err}"))?;

        // Forward stdout/stderr to flux with "db" source prefix
        let service_name = format!("{app_slug}_db");
        let app_path = app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let svc = service_name.clone();
            let path = app_path.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!(
                        "{}",
                        apx_common::format::format_process_log_line("db", &line)
                    );
                    forward_log_to_flux(&line, "INFO", &svc, &path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!(
                        "{}",
                        apx_common::format::format_process_log_line("db", &line)
                    );
                    let severity = apx_common::format::parse_python_severity(&line);
                    forward_log_to_flux(&line, severity, &service_name, &app_path).await;
                }
            });
        }

        Ok(child)
    }

    /// Poll TCP port until PGlite accepts connections.
    async fn wait_for_ready(port: u16) -> Result<(), String> {
        for _ in 0..READINESS_POLL_LIMIT {
            if tokio::net::TcpStream::connect((CLIENT_HOST, port))
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(READINESS_POLL_INTERVAL).await;
        }
        Err(format!(
            "Embedded database not ready on {CLIENT_HOST}:{port}"
        ))
    }

    /// Rotate the default PGlite password using a parameterized query.
    ///
    /// PGlite only supports one connection at a time, so the client and
    /// connection are dropped and awaited before returning.
    async fn rotate_password(port: u16, new_password: &str) -> Result<(), String> {
        use tokio_postgres::NoTls;

        let conn_str = format!(
            "host={CLIENT_HOST} port={port} user={DEFAULT_USER} password={DEFAULT_USER} dbname={DEFAULT_DB}"
        );

        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .map_err(|e| format!("Failed to connect to embedded database: {e}"))?;

        let conn_handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                warn!("Embedded database connection error: {}", e);
            }
        });

        // Parameterized query — no SQL injection risk
        let result = client
            .execute("ALTER USER postgres WITH PASSWORD $1", &[&new_password])
            .await
            .map_err(|e| format!("Failed to rotate password: {e}"));

        drop(client);

        match timeout(Duration::from_secs(5), conn_handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("Database connection task panicked: {}", e),
            Err(_) => warn!("Timed out waiting for database connection to close"),
        }

        result.map(|_| ())
    }

    /// Background task that polls the child process for early exit.
    fn spawn_health_monitor(child: Arc<Mutex<Option<Child>>>) {
        tokio::spawn(async move {
            let start_time = chrono::Utc::now();
            let timeout_duration = chrono::Duration::seconds(HEALTH_MONITOR_TIMEOUT_SECS);

            loop {
                tokio::time::sleep(HEALTH_POLL_INTERVAL).await;

                if chrono::Utc::now() - start_time > timeout_duration {
                    break;
                }

                let mut guard = child.lock().await;
                match guard.as_mut() {
                    Some(c) => match c.try_wait() {
                        Ok(Some(status)) => {
                            warn!("Embedded database exited early with status: {:?}", status);
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            warn!("Failed to check embedded database status: {}", e);
                            break;
                        }
                    },
                    None => {
                        warn!("Embedded database process handle lost");
                        break;
                    }
                }
            }
        });
    }
}
