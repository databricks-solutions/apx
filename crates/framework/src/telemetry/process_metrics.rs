//! Per-process metrics collection via `sysinfo`.
//!
//! Spawns a tokio task that periodically reads CPU, memory, and thread
//! count for the current process and reports them through the OTEL
//! metrics pipeline. Collected per-worker (and once for the supervisor).

use std::time::Duration;

use opentelemetry::metrics::Gauge;
use sysinfo::{Pid, System};

use super::config::ProcessConfig;
use super::defs;
use super::http::framework_meter;

/// Spawn the process metrics collection background task.
///
/// Returns the `JoinHandle` so the caller can abort on shutdown.
pub fn spawn_process_metrics(config: &ProcessConfig) -> tokio::task::JoinHandle<()> {
    let toggles = config.metrics;
    let interval = Duration::from_secs_f64(config.interval_secs);
    let pid = Pid::from_u32(std::process::id());

    tracing::trace!(
        name: "apx.telemetry.process_metrics.started",
        target: "apx::telemetry",
        interval_secs = config.interval_secs,
        pid = pid.as_u32(),
        "spawning process metrics collection task"
    );

    tokio::spawn(async move {
        collection_loop(toggles, interval, pid).await;
    })
}

/// Process-level instruments, created once and reused.
struct Instruments {
    process_cpu: Option<Gauge<f64>>,
    process_memory: Option<Gauge<f64>>,
    process_threads: Option<Gauge<f64>>,
}

impl Instruments {
    fn new(toggles: super::config::ProcessMetricToggles) -> Self {
        let meter = framework_meter();
        Self {
            process_cpu: defs::PROCESS_CPU.optional_gauge(&meter, toggles.process_cpu),
            process_memory: defs::PROCESS_MEMORY.optional_gauge(&meter, toggles.process_memory),
            process_threads: defs::PROCESS_THREADS.optional_gauge(&meter, toggles.process_threads),
        }
    }
}

/// Periodic collection loop.
async fn collection_loop(
    toggles: super::config::ProcessMetricToggles,
    interval: Duration,
    pid: Pid,
) {
    let instruments = Instruments::new(toggles);
    let mut sys = System::new();
    let no_attrs: &[opentelemetry::KeyValue] = &[];

    loop {
        tokio::time::sleep(interval).await;
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

        let Some(process) = sys.process(pid) else {
            continue;
        };

        if let Some(gauge) = &instruments.process_cpu {
            let usage = f64::from(process.cpu_usage()) / 100.0;
            gauge.record(usage, no_attrs);
        }
        if let Some(gauge) = &instruments.process_memory {
            gauge.record(process.memory() as f64, no_attrs);
        }
        if let Some(gauge) = &instruments.process_threads
            && let Some(tasks) = process.tasks()
        {
            gauge.record(tasks.len() as f64, no_attrs);
        }
    }
}
