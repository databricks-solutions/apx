//! Frontend (Vite/Bun) lifecycle manager for the APX dev server.
//!
//! Encapsulates bun/vite spawning, OTEL configuration, and health monitoring.
//! No bun/vite-specific details leak beyond this module.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use tokio::process::Child;
use tokio::sync::Mutex;

use crate::dev::common::{DevProcess, ProbeResult, http_health_probe};
use crate::dev::token;
use crate::external::uv::ApxTool;
use apx_common::hosts::CLIENT_HOST;

// ---------------------------------------------------------------------------
// FrontendConfig — named constructor parameters
// ---------------------------------------------------------------------------

/// All immutable and shared-state values needed to construct a [`Frontend`].
pub(crate) struct FrontendConfig {
    pub app_dir: PathBuf,
    pub app_slug: String,
    pub host: String,
    pub backend_port: u16,
    pub frontend_port: u16,
    pub db_port: u16,
    pub dev_server_port: u16,
    pub dev_token: String,
}

// ---------------------------------------------------------------------------
// Frontend
// ---------------------------------------------------------------------------

/// Self-contained frontend (Vite/Bun) lifecycle manager.
/// `ProcessManager` interacts only through this API.
pub(crate) struct Frontend {
    child: Arc<Mutex<Option<Child>>>,
    cfg: FrontendConfig,
}

// `Child` does not implement `Debug`, so we provide a manual impl.
impl std::fmt::Debug for Frontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frontend")
            .field("app_slug", &self.cfg.app_slug)
            .field("frontend_port", &self.cfg.frontend_port)
            .finish()
    }
}

impl Frontend {
    pub fn new(cfg: FrontendConfig) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cfg,
        }
    }

    /// Spawn the frontend dev server (`apx frontend dev` via uv).
    ///
    /// Frontend logs are NOT piped through apx stdout/stderr — the frontend
    /// process sends logs directly to flux via OTEL SDK. See entrypoint.ts.
    pub async fn spawn(&self) -> Result<(), String> {
        let cmd = self.build_command().await?;
        let child = cmd.spawn().map_err(String::from)?;

        let mut guard = self.child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    // -- private: command construction --

    /// Build the `apx frontend dev` command with all env vars and OTEL config.
    async fn build_command(&self) -> Result<crate::external::ToolCommand, String> {
        let cfg = &self.cfg;

        let cmd = ApxTool::new_apx()
            .await?
            .cmd()
            .args(["frontend", "dev"])
            .cwd(&cfg.app_dir)
            .stdin(Stdio::null())
            // Inherit stdout/stderr for local visibility, but don't capture/forward
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            // APX runtime context
            .env("APX_BACKEND_PORT", cfg.backend_port.to_string())
            .env("APX_DEV_DB_PORT", cfg.db_port.to_string())
            .env("APX_DEV_SERVER_PORT", cfg.dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", &cfg.host)
            .env(token::DEV_TOKEN_ENV, &cfg.dev_token)
            .env("APX_APP_NAME", &cfg.app_slug)
            .env("APX_APP_PATH", cfg.app_dir.display().to_string())
            .env("APX_FRONTEND_PORT", cfg.frontend_port.to_string())
            // OpenTelemetry configuration — frontend sends logs directly to flux
            .env(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                format!("http://{}:{}", CLIENT_HOST, crate::flux::FLUX_PORT),
            )
            .env(apx_common::hosts::ENV_FRONTEND_HOST, CLIENT_HOST)
            .env("OTEL_SERVICE_NAME", format!("{}_ui", cfg.app_slug));

        Ok(cmd)
    }
}

// ---------------------------------------------------------------------------
// DevProcess impl
// ---------------------------------------------------------------------------

impl DevProcess for Frontend {
    fn child_handle(&self) -> &Arc<Mutex<Option<Child>>> {
        &self.child
    }

    fn label(&self) -> &'static str {
        "frontend"
    }

    async fn status(&self) -> &'static str {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            None => return "stopped",
            Some(process) => match process.try_wait() {
                Ok(None) => {} // still running — continue to HTTP probe
                Ok(Some(_)) => return "failed",
                Err(_) => return "error",
            },
        }
        drop(guard);

        match http_health_probe(CLIENT_HOST, self.cfg.frontend_port).await {
            ProbeResult::Responded(_) => "healthy",
            ProbeResult::Failed(_) => "starting",
        }
    }
}
