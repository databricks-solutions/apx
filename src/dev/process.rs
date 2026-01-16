use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use rand::{distributions::Alphanumeric, Rng};
use sysinfo::{Pid, Signal, System};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

use crate::bun_binary_path;
use crate::common::read_project_metadata;
use crate::dev::logging::{LogPayload, LogPipe, LogQueue, LogStreamName};

#[derive(Debug, Clone, Copy)]
enum LogSource {
    App,
    Ui,
}

impl fmt::Display for LogSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogSource::App => write!(f, "app"),
            LogSource::Ui => write!(f, "ui"),
        }
    }
}

impl From<LogSource> for LogStreamName {
    fn from(source: LogSource) -> Self {
        match source {
            LogSource::App => LogStreamName::App,
            LogSource::Ui => LogStreamName::Ui,
        }
    }
}

#[derive(Debug)]
pub struct ProcessManager {
    log_queue: LogQueue,
    frontend_child: Arc<Mutex<Option<Child>>>,
    backend_child: Arc<Mutex<Option<Child>>>,
    backend_port: u16,
    frontend_port: u16,
    dev_server_port: u16,
    host: String,
    dev_token: String,
}

impl ProcessManager {
    pub async fn start(
        app_dir: &Path,
        host: &str,
        dev_server_port: u16,
        backend_port: u16,
        frontend_port: u16,
    ) -> Result<Self, String> {
        let metadata = read_project_metadata(app_dir)?;
        let bun_path = bun_binary_path()?;
        if !bun_path.exists() {
            return Err("bun is not installed. Please install bun to continue.".to_string());
        }

        let dev_token = Self::generate_dev_token();
        let manager = Self {
            log_queue: Arc::new(Mutex::new(VecDeque::new())),
            frontend_child: Arc::new(Mutex::new(None)),
            backend_child: Arc::new(Mutex::new(None)),
            backend_port,
            frontend_port,
            dev_server_port,
            host: host.to_string(),
            dev_token,
        };

        manager.spawn_bun_dev(app_dir, bun_path).await?;
        manager
            .spawn_uvicorn(app_dir, metadata.app_module)
            .await?;

        Ok(manager)
    }

    pub async fn stop(&self) {
        debug!(
            host = %self.host,
            frontend_port = self.frontend_port,
            backend_port = self.backend_port,
            dev_server_port = self.dev_server_port,
            "Stopping dev processes."
        );
        Self::stop_child_tree("backend", &self.backend_child).await;
        debug!("Backend child stop attempted.");
        Self::stop_child_tree("frontend", &self.frontend_child).await;
        debug!("Frontend child stop attempted.");
    }

    pub async fn status(&self) -> (String, String) {
        let frontend_status = Self::status_for_child(&self.frontend_child).await;
        let backend_status = Self::status_for_child(&self.backend_child).await;
        (frontend_status, backend_status)
    }

    async fn spawn_bun_dev(&self, app_dir: &Path, bun_path: PathBuf) -> Result<(), String> {
        let child = self.spawn_process(
            app_dir,
            bun_path,
            vec!["run".to_string(), "dev".to_string()],
            LogSource::Ui,
        )
        .await
        ?;
        let mut guard = self.frontend_child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn spawn_uvicorn(
        &self,
        app_dir: &Path,
        app_module: String,
    ) -> Result<(), String> {
        let child = self.spawn_process(
            app_dir,
            PathBuf::from("uv"),
            vec![
                "run".to_string(),
                "uvicorn".to_string(),
                app_module,
                "--host".to_string(),
                self.host.clone(),
                "--port".to_string(),
                self.backend_port.to_string(),
                "--reload".to_string(),
            ],
            LogSource::App,
        )
        .await
        ?;
        let mut guard = self.backend_child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn spawn_process(
        &self,
        app_dir: &Path,
        executable: PathBuf,
        args: Vec<String>,
        source: LogSource,
    ) -> Result<Child, String> {
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .current_dir(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_env(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|err| format!("Failed to start {source} process: {err}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("Failed to capture {source} stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("Failed to capture {source} stderr"))?;

        self.spawn_log_reader(stdout, source, LogPipe::Out);
        self.spawn_log_reader(stderr, source, LogPipe::Error);

        Ok(child)
    }

    fn spawn_log_reader<R>(&self, reader: R, source: LogSource, pipe: LogPipe)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let log_queue = Arc::clone(&self.log_queue);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let entry = LogPayload {
                    stream: LogStreamName::from(source),
                    pipe: Some(pipe),
                    message: line,
                };
                let mut queue = log_queue.lock().await;
                queue.push_back(entry);
            }
        });
    }

    pub async fn drain_logs(&self) -> Vec<LogPayload> {
        let mut guard = self.log_queue.lock().await;
        guard.drain(..).collect()
    }

    pub async fn is_log_queue_empty(&self) -> bool {
        let guard = self.log_queue.lock().await;
        guard.is_empty()
    }

    pub async fn is_shutdown_complete(&self) -> bool {
        let frontend_status = Self::status_for_child(&self.frontend_child).await;
        let backend_status = Self::status_for_child(&self.backend_child).await;
        frontend_status == "stopped" && backend_status == "stopped"
    }

    fn apply_env(&self, cmd: &mut Command) {
        cmd.env("APX_FRONTEND_PORT", self.frontend_port.to_string());
        cmd.env("APX_BACKEND_PORT", self.backend_port.to_string());
        cmd.env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string());
        cmd.env("APX_DEV_SERVER_HOST", self.host.clone());
        cmd.env("APX_DEV_TOKEN", self.dev_token.clone());
    }

    async fn stop_child_tree(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            if let Some(pid) = pid {
                if let Err(err) = Self::kill_process_tree(pid, name) {
                    warn!(error = %err, process = name, pid, "Failed to kill process tree.");
                }
            } else {
                warn!(process = name, "Missing PID for child process.");
            }
            match timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => debug!(process = name, ?status, "Child process exited."),
                Ok(Err(err)) => warn!(error = %err, process = name, "Failed to wait for child process."),
                Err(_) => warn!(process = name, "Timed out waiting for child process to exit."),
            }
        } else {
            debug!(process = name, "No child process to stop.");
        }
    }

    async fn status_for_child(child: &Arc<Mutex<Option<Child>>>) -> String {
        let mut guard = child.lock().await;
        match guard.as_mut() {
            None => "stopped".to_string(),
            Some(process) => match process.try_wait() {
                Ok(None) => "healthy".to_string(),
                Ok(Some(_)) => "stopped".to_string(),
                Err(_) => "error".to_string(),
            },
        }
    }

    fn generate_dev_token() -> String {
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect()
    }

    pub fn kill_process_tree(pid: u32, label: &str) -> Result<(), String> {
        let root_pid = Pid::from_u32(pid);
        let mut sys = System::new_all();
        sys.refresh_all();
        let root_process = sys
            .process(root_pid)
            .ok_or_else(|| format!("{label} process {pid} not found"))?;
        let root_start_time = root_process.start_time();
        let parents = Self::build_parent_map(&sys);
        debug!(
            pid = ?root_pid,
            root_start_time,
            process = label,
            "Killing process tree."
        );
        Self::kill_tree_with_guard(&sys, &parents, root_pid, root_start_time, label);
        Ok(())
    }

    fn build_parent_map(sys: &System) -> HashMap<Pid, Vec<Pid>> {
        let mut parents: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in sys.processes() {
            if let Some(parent) = process.parent() {
                parents.entry(parent).or_default().push(*pid);
            }
        }
        parents
    }

    fn kill_tree_with_guard(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        label: &str,
    ) {
        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::kill_tree_with_guard(sys, parents, *child_pid, root_start_time, label);
            }
        }

        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time < root_start_time {
                debug!(
                    pid = ?pid,
                    process_start_time,
                    root_start_time,
                    process = label,
                    "Skipping process because it predates the root."
                );
                return;
            }
            let name = process.name();
            let killed = process
                .kill_with(Signal::Kill)
                .unwrap_or(false);
            if killed {
                debug!(pid = ?pid, process_name = ?name, process = label, "Killed process.");
            } else {
                warn!(pid = ?pid, process_name = ?name, process = label, "Failed to kill process.");
            }
        }
    }
}
