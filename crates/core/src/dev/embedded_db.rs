//! Embedded database lifecycle manager for the APX dev server.
//!
//! Encapsulates PGlite spawning, readiness polling, credential rotation,
//! and health monitoring. No PGlite-specific details leak beyond this module.
// Runs inside the dev server process (in-process for attached mode,
// child process for detached mode). Never in the MCP server process
// — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{debug, warn};

use crate::dev::common::DevProcess;
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
const HEALTH_MONITOR_TIMEOUT: Duration = Duration::from_secs(60);

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

impl EmbeddedDb {
    /// Spawn PGlite via bun, wait for PG protocol readiness, rotate the
    /// default password, and start a background health monitor.
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

    #[allow(dead_code)]
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

    /// Poll until PGlite accepts PostgreSQL wire-protocol connections.
    ///
    /// PGlite opens its TCP listener before the PG protocol handler is ready,
    /// so a raw TCP check is insufficient. We attempt a full
    /// `tokio_postgres::connect` handshake to confirm the database is truly
    /// ready before proceeding to password rotation.
    async fn wait_for_ready(port: u16) -> Result<(), String> {
        use tokio_postgres::NoTls;

        let conn_str = format!(
            "host={CLIENT_HOST} port={port} user={DEFAULT_USER} password={DEFAULT_USER} dbname={DEFAULT_DB}"
        );

        for _ in 0..READINESS_POLL_LIMIT {
            if let Ok((_, connection)) = tokio_postgres::connect(&conn_str, NoTls).await {
                // PGlite is single-connection: drive the connection future to
                // completion so PGlite releases the exclusive lock before
                // rotate_password opens its own connection.
                let _ = timeout(Duration::from_secs(2), connection).await;
                return Ok(());
            }
            tokio::time::sleep(READINESS_POLL_INTERVAL).await;
        }
        Err(format!(
            "Embedded database not ready on {CLIENT_HOST}:{port}"
        ))
    }

    /// Rotate the default PGlite password via the simple query protocol.
    ///
    /// PGlite's socket wrapper does not support the extended query protocol
    /// (Parse/Bind/Execute) for DDL statements, so we use `batch_execute`
    /// which sends a simple Query message instead.
    ///
    /// The password is alphanumeric (generated by `token::generate`), so
    /// SQL injection is not a concern. Single quotes are escaped as a
    /// defense-in-depth measure.
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

        // Simple query protocol — PGlite does not support extended protocol for DDL.
        // Password is alphanumeric; escape single quotes as defense-in-depth.
        let escaped = new_password.replace('\'', "''");
        let result = client
            .batch_execute(&format!("ALTER USER postgres WITH PASSWORD '{escaped}'"))
            .await
            .map_err(|e| format!("Failed to rotate password: {e}"));

        drop(client);

        match timeout(Duration::from_secs(5), conn_handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("Database connection task panicked: {}", e),
            Err(_) => warn!("Timed out waiting for database connection to close"),
        }

        result
    }

    /// Background task that polls the child process for early exit.
    fn spawn_health_monitor(child: Arc<Mutex<Option<Child>>>) {
        tokio::spawn(async move {
            let start = tokio::time::Instant::now();

            loop {
                tokio::time::sleep(HEALTH_POLL_INTERVAL).await;

                if start.elapsed() > HEALTH_MONITOR_TIMEOUT {
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

impl DevProcess for EmbeddedDb {
    fn child_handle(&self) -> &Arc<Mutex<Option<Child>>> {
        &self.child
    }

    fn label(&self) -> &'static str {
        "db"
    }

    async fn status(&self) -> &'static str {
        EmbeddedDb::status(self).await
    }
}
