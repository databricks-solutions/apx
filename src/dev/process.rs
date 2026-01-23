use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rand::{distributions::Alphanumeric, Rng};
use sysinfo::{Pid, Signal, System};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

use crate::bun_binary_path;
use crate::common::read_project_metadata;
use crate::dev::logging::{
    log_queue_since, log_queue_since_timestamp, push_log, LogPayload, LogPipe, LogQueue,
    LogStreamName,
};
use crate::dotenv::DotenvFile;

#[derive(Debug, Clone, Copy)]
enum LogSource {
    App,
    Ui,
    Db,
}

impl fmt::Display for LogSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogSource::App => write!(f, "app"),
            LogSource::Ui => write!(f, "ui"),
            LogSource::Db => write!(f, "db"),
        }
    }
}

impl From<LogSource> for LogStreamName {
    fn from(source: LogSource) -> Self {
        match source {
            LogSource::App => LogStreamName::App,
            LogSource::Ui => LogStreamName::Ui,
            LogSource::Db => LogStreamName::Db,
        }
    }
}

#[derive(Debug)]
pub struct ProcessManager {
    log_queue: LogQueue,
    frontend_child: Arc<Mutex<Option<Child>>>,
    backend_child: Arc<Mutex<Option<Child>>>,
    db_child: Arc<Mutex<Option<Child>>>,
    backend_port: u16,
    frontend_port: u16,
    db_port: u16,
    dev_server_port: u16,
    host: String,
    dev_token: String,
    app_dir: PathBuf,
    app_module: String,
    dotenv_vars: Arc<Mutex<HashMap<String, String>>>,
}

impl ProcessManager {
    pub async fn start(
        app_dir: &Path,
        host: &str,
        dev_server_port: u16,
        backend_port: u16,
        frontend_port: u16,
        db_port: u16,
    ) -> Result<Self, String> {
        let metadata = read_project_metadata(app_dir)?;
        let bun_path = Self::ensure_bun_path()?;

        let dotenv = DotenvFile::read(&app_dir.join(".env"))?;
        let dotenv_vars = Arc::new(Mutex::new(dotenv.get_vars()));
        let app_module = metadata.app_module.clone();

        let dev_token = Self::generate_dev_token();
        let manager = Self {
            log_queue: Arc::new(Mutex::new(Vec::new())),
            frontend_child: Arc::new(Mutex::new(None)),
            backend_child: Arc::new(Mutex::new(None)),
            db_child: Arc::new(Mutex::new(None)),
            backend_port,
            frontend_port,
            db_port,
            dev_server_port,
            host: host.to_string(),
            dev_token,
            app_dir: app_dir.to_path_buf(),
            app_module,
            dotenv_vars,
        };

        debug!(
            "Spawning bun dev process"
        );
        manager.spawn_bun_dev(app_dir, bun_path.clone()).await?;
        debug!(
            "Spawning PGLite database process"
        );
        manager.spawn_pglite(&bun_path).await?;
        debug!(
            "Spawning uvicorn process"
        );
        manager
            .spawn_uvicorn(app_dir, metadata.app_module)
            .await?;

        debug!(
            "Starting file watcher for backend restart"
        );
        manager.start_backend_file_watcher();

        debug!(
            "Frontend and backend processes spawned"
        );
        Ok(manager)
    }

    pub fn dev_token(&self) -> &str {
        &self.dev_token
    }

    /// Stop all managed processes using a phased shutdown approach:
    /// 1. Send SIGTERM to allow graceful shutdown
    /// 2. Wait briefly for processes to exit
    /// 3. Force kill any remaining processes
    pub async fn stop(&self) {
        debug!(
            host = %self.host,
            frontend_port = self.frontend_port,
            backend_port = self.backend_port,
            db_port = self.db_port,
            dev_server_port = self.dev_server_port,
            "Stopping dev processes with phased shutdown."
        );

        // Phase 1: Send SIGTERM to all processes (polite request to stop)
        debug!("Phase 1: Sending SIGTERM to all processes.");
        Self::send_sigterm("backend", &self.backend_child).await;
        Self::send_sigterm("frontend", &self.frontend_child).await;
        Self::send_sigterm("db", &self.db_child).await;

        // Phase 2: Wait briefly for graceful exit (500ms)
        debug!("Phase 2: Waiting for graceful exit.");
        let wait_backend = Self::wait_for_child("backend", &self.backend_child);
        let wait_frontend = Self::wait_for_child("frontend", &self.frontend_child);
        let wait_db = Self::wait_for_child("db", &self.db_child);
        let _ = timeout(Duration::from_millis(500), async {
            tokio::join!(wait_backend, wait_frontend, wait_db)
        })
        .await;

        // Phase 3: Force kill any remaining processes
        debug!("Phase 3: Force killing remaining processes.");
        Self::force_kill("backend", &self.backend_child).await;
        Self::force_kill("frontend", &self.frontend_child).await;
        Self::force_kill("db", &self.db_child).await;

        debug!("All processes stopped.");
    }

    pub async fn status(&self) -> (String, String, String) {
        let one_minute_ago = chrono::Utc::now().timestamp_millis() - 60_000;
        let (_, logs) = self.logs_since_timestamp(one_minute_ago).await;

        let frontend_status = Self::status_for_child(&self.frontend_child, &logs, LogSource::Ui).await;
        let backend_status = Self::status_for_child(&self.backend_child, &logs, LogSource::App).await;
        let db_status = Self::status_for_child(&self.db_child, &logs, LogSource::Db).await;
        (frontend_status, backend_status, db_status)
    }

    pub async fn restart_uvicorn_with_env(
        &self,
        new_vars: HashMap<String, String>,
    ) -> Result<(), String> {
        Self::stop_child_tree("backend", &self.backend_child).await;
        {
            let mut vars = self.dotenv_vars.lock().await;
            *vars = new_vars;
        }
        self.spawn_uvicorn(&self.app_dir, self.app_module.clone())
            .await
    }

    async fn spawn_bun_dev(&self, app_dir: &Path, bun_path: PathBuf) -> Result<(), String> {
        let child = self.spawn_process(
            app_dir,
            bun_path,
            vec!["run".to_string(), "dev".to_string()],
            LogSource::Ui,
            false,
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
            true,
        )
        .await
        ?;
        let mut guard = self.backend_child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn spawn_pglite(&self, bun_path: &Path) -> Result<(), String> {
        let child = self.spawn_process(
            &self.app_dir,
            bun_path.to_path_buf(),
            vec![
                "x".to_string(),
                "@electric-sql/pglite-socket".to_string(),
                "--db=memory://".to_string(),
                format!("--host={}", self.host),
                "--debug=0".to_string(),
                format!("--port={}", self.db_port),
            ],
            LogSource::Db,
            false,
        )
        .await?;

        let mut guard = self.db_child.lock().await;
        *guard = Some(child);

        self.spawn_db_health_monitor();
        Ok(())
    }

    fn spawn_db_health_monitor(&self) {
        let db_child = Arc::clone(&self.db_child);
        tokio::spawn(async move {
            let start_time = chrono::Utc::now();
            let timeout_duration = chrono::Duration::seconds(60);

            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let elapsed = chrono::Utc::now() - start_time;

                if elapsed > timeout_duration {
                    break;
                }

                let mut guard = db_child.lock().await;
                if let Some(child) = guard.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            warn!("PGLite process exited early with status: {:?}", status);
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            warn!("Failed to check PGLite process status: {}", e);
                            break;
                        }
                    }
                } else {
                    warn!("PGLite process handle lost");
                    break;
                }
            }
        });
    }

    fn start_backend_file_watcher(&self) {
        let app_dir = self.app_dir.clone();
        let dotenv_vars = Arc::clone(&self.dotenv_vars);
        let backend_child = Arc::clone(&self.backend_child);
        let log_queue = Arc::clone(&self.log_queue);
        let app_module = self.app_module.clone();
        let host = self.host.clone();
        let backend_port = self.backend_port;
        let frontend_port = self.frontend_port;
        let db_port = self.db_port;
        let dev_server_port = self.dev_server_port;
        let dev_token = self.dev_token.clone();

        tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(100);

            let mut watcher = match RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    if let Ok(event) = res {
                        let _ = tx.blocking_send(event);
                    }
                },
                notify::Config::default(),
            ) {
                Ok(w) => w,
                Err(e) => {
                    warn!("Failed to create file watcher: {}", e);
                    return;
                }
            };

            let watched_files = vec![
                app_dir.join(".env"),
                app_dir.join("pyproject.toml"),
                app_dir.join("uv.lock"),
            ];

            for file in &watched_files {
                if file.exists() {
                    if let Err(e) = watcher.watch(file, RecursiveMode::NonRecursive) {
                        warn!("Failed to watch file {:?}: {}", file, e);
                    }
                }
            }

            while let Some(event) = rx.recv().await {
                if !matches!(
                    event.kind,
                    notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                ) {
                    continue;
                }

                for path in &event.paths {
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    if !["pyproject.toml", "uv.lock", ".env"].contains(&file_name) {
                        continue;
                    }

                    info!("File change detected: {} - restarting backend", file_name);

                    // Reload .env if it exists
                    let new_vars = if let Ok(dotenv) = DotenvFile::read(&app_dir.join(".env")) {
                        dotenv.get_vars()
                    } else {
                        HashMap::new()
                    };

                    // Stop the current backend process
                    Self::stop_child_tree_static("backend", &backend_child).await;

                    // Update dotenv vars
                    {
                        let mut vars = dotenv_vars.lock().await;
                        *vars = new_vars.clone();
                    }

                    // Restart uvicorn
                    match Self::spawn_uvicorn_static(
                        &app_dir,
                        &app_module,
                        &host,
                        backend_port,
                        frontend_port,
                        db_port,
                        dev_server_port,
                        &dev_token,
                        &dotenv_vars,
                        &backend_child,
                        &log_queue,
                    )
                    .await
                    {
                        Ok(_) => info!("Backend restarted successfully"),
                        Err(e) => warn!("Failed to restart backend: {}", e),
                    }

                    // Only process one file change at a time
                    break;
                }
            }
        });
    }

    async fn spawn_process(
        &self,
        app_dir: &Path,
        executable: PathBuf,
        args: Vec<String>,
        source: LogSource,
        include_dotenv: bool,
    ) -> Result<Child, String> {
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .current_dir(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_env(&mut cmd, include_dotenv).await;

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

    /// Static version of stop_child_tree for use in async tasks without self
    async fn stop_child_tree_static(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            if let Some(pid) = pid {
                if let Err(err) = Self::kill_process_tree_async(pid, name.to_string()).await {
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

    /// Static version of spawn_uvicorn for use in async tasks without self
    async fn spawn_uvicorn_static(
        app_dir: &Path,
        app_module: &str,
        host: &str,
        backend_port: u16,
        frontend_port: u16,
        db_port: u16,
        dev_server_port: u16,
        dev_token: &str,
        dotenv_vars: &Arc<Mutex<HashMap<String, String>>>,
        backend_child: &Arc<Mutex<Option<Child>>>,
        log_queue: &LogQueue,
    ) -> Result<(), String> {
        let mut cmd = Command::new("uv");
        cmd.args([
            "run",
            "uvicorn",
            app_module,
            "--host",
            host,
            "--port",
            &backend_port.to_string(),
            "--reload",
        ])
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        // Apply environment variables
        cmd.env("APX_FRONTEND_PORT", frontend_port.to_string());
        cmd.env("APX_BACKEND_PORT", backend_port.to_string());
        cmd.env("APX_DEV_DB_PORT", db_port.to_string());
        cmd.env("APX_DEV_SERVER_PORT", dev_server_port.to_string());
        cmd.env("APX_DEV_SERVER_HOST", host);
        cmd.env("APX_DEV_TOKEN", dev_token);

        let vars = dotenv_vars.lock().await;
        for (key, value) in vars.iter() {
            cmd.env(key, value);
        }
        drop(vars);

        let mut child = cmd
            .spawn()
            .map_err(|err| format!("Failed to start app process: {err}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture app stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture app stderr".to_string())?;

        // Spawn log readers
        let log_queue_stdout = Arc::clone(log_queue);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let entry = LogPayload::new(LogStreamName::App, Some(LogPipe::Out), line);
                push_log(&log_queue_stdout, entry).await;
            }
        });

        let log_queue_stderr = Arc::clone(log_queue);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let entry = LogPayload::new(LogStreamName::App, Some(LogPipe::Error), line);
                push_log(&log_queue_stderr, entry).await;
            }
        });

        let mut guard = backend_child.lock().await;
        *guard = Some(child);

        Ok(())
    }

    fn spawn_log_reader<R>(&self, reader: R, source: LogSource, pipe: LogPipe)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let log_queue = Arc::clone(&self.log_queue);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let entry = LogPayload::new(LogStreamName::from(source), Some(pipe), line);
                push_log(&log_queue, entry).await;
            }
        });
    }

    pub async fn logs_since_index(&self, start_index: usize) -> (usize, Vec<LogPayload>) {
        log_queue_since(&self.log_queue, start_index).await
    }

    pub async fn logs_since_timestamp(&self, since: i64) -> (usize, Vec<LogPayload>) {
        log_queue_since_timestamp(&self.log_queue, since).await
    }

    pub async fn push_browser_log(&self, payload: LogPayload) {
        push_log(&self.log_queue, payload).await;
    }

    async fn apply_env(&self, cmd: &mut Command, include_dotenv: bool) {
        cmd.env("APX_FRONTEND_PORT", self.frontend_port.to_string());
        cmd.env("APX_BACKEND_PORT", self.backend_port.to_string());
        cmd.env("APX_DEV_DB_PORT", self.db_port.to_string());
        cmd.env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string());
        cmd.env("APX_DEV_SERVER_HOST", self.host.clone());
        cmd.env("APX_DEV_TOKEN", self.dev_token.clone());
        if include_dotenv {
            let vars = self.dotenv_vars.lock().await;
            for (key, value) in vars.iter() {
                cmd.env(key, value);
            }
        }
    }

    fn ensure_bun_path() -> Result<PathBuf, String> {
        let bun_path = bun_binary_path()?;
        if !bun_path.exists() {
            return Err("bun is not installed. Please install bun to continue.".to_string());
        }
        Ok(bun_path)
    }

    /// Send SIGTERM to a child process tree (polite shutdown request).
    async fn send_sigterm(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let guard = child.lock().await;
        if let Some(child) = guard.as_ref() {
            if let Some(pid) = child.id() {
                debug!(process = name, pid, "Sending SIGTERM to process tree.");
                Self::send_signal_to_tree(pid, Signal::Term, name.to_string()).await;
            }
        }
    }

    /// Wait for a child process to exit.
    async fn wait_for_child(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    debug!(process = name, ?status, "Child process already exited.");
                }
                Ok(None) => {
                    // Process still running, wait for it
                    match child.wait().await {
                        Ok(status) => debug!(process = name, ?status, "Child process exited."),
                        Err(err) => warn!(error = %err, process = name, "Failed to wait for child."),
                    }
                }
                Err(err) => warn!(error = %err, process = name, "Failed to check child status."),
            }
        }
    }

    /// Force kill a child process tree (SIGKILL).
    async fn force_kill(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            // Check if process is still running
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Already exited, nothing to do
                    debug!(process = name, "Process already exited, skipping force kill.");
                }
                Ok(None) => {
                    // Still running, force kill
                    if let Some(pid) = child.id() {
                        debug!(process = name, pid, "Force killing process tree.");
                        Self::send_signal_to_tree(pid, Signal::Kill, name.to_string()).await;
                        // Brief wait to allow kill to take effect
                        let _ = timeout(Duration::from_millis(100), child.wait()).await;
                    }
                }
                Err(err) => {
                    warn!(error = %err, process = name, "Failed to check process status.");
                }
            }
        }
    }

    /// Send a signal to an entire process tree. This is a blocking operation.
    fn send_signal_to_tree_blocking(pid: u32, signal: Signal, label: &str) {
        let root_pid = Pid::from_u32(pid);
        let mut sys = System::new_all();
        sys.refresh_all();

        let Some(root_process) = sys.process(root_pid) else {
            debug!(process = label, pid, "Process not found, may have already exited.");
            return;
        };

        let root_start_time = root_process.start_time();
        let parents = Self::build_parent_map(&sys);
        Self::send_signal_tree_recursive(&sys, &parents, root_pid, root_start_time, signal, label);
    }

    /// Async wrapper for send_signal_to_tree that runs on a blocking thread.
    async fn send_signal_to_tree(pid: u32, signal: Signal, label: String) {
        let _ = tokio::task::spawn_blocking(move || {
            Self::send_signal_to_tree_blocking(pid, signal, &label)
        })
        .await;
    }

    /// Recursively send signal to process tree.
    fn send_signal_tree_recursive(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        signal: Signal,
        label: &str,
    ) {
        // First, signal all children
        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::send_signal_tree_recursive(
                    sys,
                    parents,
                    *child_pid,
                    root_start_time,
                    signal,
                    label,
                );
            }
        }

        // Then signal this process
        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time < root_start_time {
                return;
            }
            let name = process.name();
            if process.kill_with(signal).unwrap_or(false) {
                debug!(pid = ?pid, process_name = ?name, ?signal, process = label, "Sent signal to process.");
            }
        }
    }

    /// Stop a child process tree immediately (used for restart operations).
    async fn stop_child_tree(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            if let Some(pid) = pid {
                if let Err(err) = Self::kill_process_tree_async(pid, name.to_string()).await {
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

    async fn status_for_child(
        child: &Arc<Mutex<Option<Child>>>,
        logs: &[LogPayload],
        source: LogSource,
    ) -> String {
        let mut guard = child.lock().await;
        match guard.as_mut() {
            None => "stopped".to_string(),
            Some(process) => match process.try_wait() {
                Ok(None) => {
                    // Process is running, check for errors in logs
                    let has_errors = logs.iter().any(|log| {
                        log.stream == LogStreamName::from(source) && log.pipe == Some(LogPipe::Error)
                    });
                    
                    if has_errors {
                        "degraded, please check the logs".to_string()
                    } else {
                        "healthy".to_string()
                    }
                }
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

    /// Kill a process tree. This is a blocking operation that should be called
    /// from a blocking context or wrapped in spawn_blocking.
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

    /// Async wrapper for kill_process_tree that runs on a blocking thread.
    pub async fn kill_process_tree_async(pid: u32, label: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || Self::kill_process_tree(pid, &label))
            .await
            .map_err(|err| format!("Failed to spawn blocking task: {err}"))?
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
