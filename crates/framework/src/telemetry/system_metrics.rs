//! Background system metrics collection via `sysinfo`.
//!
//! Spawns a tokio task that periodically reads CPU, memory, swap, disk,
//! and network metrics and reports them through the OTEL metrics pipeline.
//! Only collects metrics present in `SystemConfig.metrics`.

use std::time::Duration;

use opentelemetry::KeyValue;
use sysinfo::{Disks, Networks, Pid, System};

use super::config::{SystemConfig, SystemMetricKind};
use super::http::framework_meter;

/// Spawn the system metrics collection background task.
///
/// Returns the `JoinHandle` so the caller can abort on shutdown.
pub fn spawn_system_metrics(config: &SystemConfig) -> tokio::task::JoinHandle<()> {
    let metrics = config.metrics.clone();
    let interval = Duration::from_secs_f64(config.interval_secs);
    let pid = Pid::from_u32(std::process::id());

    tracing::trace!(
        target: "apx::telemetry",
        metric_count = metrics.len(),
        interval_secs = config.interval_secs,
        pid = pid.as_u32(),
        "spawning system metrics collection task"
    );

    tokio::spawn(async move {
        collection_loop(metrics, interval, pid).await;
    })
}

/// Metrics collection instruments, created once and reused.
struct Instruments {
    process_cpu: Option<opentelemetry::metrics::Gauge<f64>>,
    system_cpu: Option<opentelemetry::metrics::Gauge<f64>>,
    system_memory: Option<opentelemetry::metrics::Gauge<f64>>,
    system_swap: Option<opentelemetry::metrics::Gauge<f64>>,
    process_memory: Option<opentelemetry::metrics::Gauge<f64>>,
    process_threads: Option<opentelemetry::metrics::Gauge<f64>>,
    disk_io: Option<opentelemetry::metrics::Gauge<f64>>,
    network_io: Option<opentelemetry::metrics::Gauge<f64>>,
}

impl Instruments {
    fn new(metrics: &std::collections::HashSet<SystemMetricKind>) -> Self {
        let meter = framework_meter();
        tracing::trace!(
            target: "apx::telemetry",
            process_cpu = metrics.contains(&SystemMetricKind::ProcessCpu),
            system_cpu = metrics.contains(&SystemMetricKind::SystemCpu),
            system_memory = metrics.contains(&SystemMetricKind::SystemMemory),
            system_swap = metrics.contains(&SystemMetricKind::SystemSwap),
            process_memory = metrics.contains(&SystemMetricKind::ProcessMemory),
            process_threads = metrics.contains(&SystemMetricKind::ProcessThreads),
            disk_io = metrics.contains(&SystemMetricKind::SystemDiskIo),
            network_io = metrics.contains(&SystemMetricKind::SystemNetworkIo),
            "creating system metric instruments"
        );
        Self {
            process_cpu: metrics.contains(&SystemMetricKind::ProcessCpu).then(|| {
                meter
                    .f64_gauge("process.cpu.utilization")
                    .with_description("Process CPU utilization as a fraction of one core")
                    .with_unit("1")
                    .build()
            }),
            system_cpu: metrics.contains(&SystemMetricKind::SystemCpu).then(|| {
                meter
                    .f64_gauge("system.cpu.simple_utilization")
                    .with_description("System-wide CPU utilization as a fraction")
                    .with_unit("1")
                    .build()
            }),
            system_memory: metrics.contains(&SystemMetricKind::SystemMemory).then(|| {
                meter
                    .f64_gauge("system.memory.utilization")
                    .with_description("Fraction of available memory used")
                    .with_unit("1")
                    .build()
            }),
            system_swap: metrics.contains(&SystemMetricKind::SystemSwap).then(|| {
                meter
                    .f64_gauge("system.swap.utilization")
                    .with_description("Fraction of swap space used")
                    .with_unit("1")
                    .build()
            }),
            process_memory: metrics.contains(&SystemMetricKind::ProcessMemory).then(|| {
                meter
                    .f64_gauge("process.memory.usage")
                    .with_description("Process resident memory in bytes")
                    .with_unit("By")
                    .build()
            }),
            process_threads: metrics
                .contains(&SystemMetricKind::ProcessThreads)
                .then(|| {
                    meter
                        .f64_gauge("process.thread.count")
                        .with_description("Number of threads in the process")
                        .with_unit("1")
                        .build()
                }),
            disk_io: metrics.contains(&SystemMetricKind::SystemDiskIo).then(|| {
                meter
                    .f64_gauge("system.disk.io")
                    .with_description("Cumulative disk I/O in bytes")
                    .with_unit("By")
                    .build()
            }),
            network_io: metrics
                .contains(&SystemMetricKind::SystemNetworkIo)
                .then(|| {
                    meter
                        .f64_gauge("system.network.io")
                        .with_description("Cumulative network I/O in bytes")
                        .with_unit("By")
                        .build()
                }),
        }
    }
}

/// Periodic collection loop.
async fn collection_loop(
    metrics: std::collections::HashSet<SystemMetricKind>,
    interval: Duration,
    pid: Pid,
) {
    let instruments = Instruments::new(&metrics);
    let mut sys = System::new();
    let needs_disks = metrics.contains(&SystemMetricKind::SystemDiskIo);
    let needs_network = metrics.contains(&SystemMetricKind::SystemNetworkIo);
    let mut disks = if needs_disks {
        Some(Disks::new())
    } else {
        None
    };
    let mut networks = if needs_network {
        Some(Networks::new())
    } else {
        None
    };

    let no_attrs: &[KeyValue] = &[];
    let mut first_tick = true;

    loop {
        tokio::time::sleep(interval).await;
        if first_tick {
            tracing::trace!(
                target: "apx::telemetry",
                interval_ms = interval.as_millis(),
                "system metrics collection loop running — first tick"
            );
            first_tick = false;
        }
        collect_once(
            &mut sys,
            &instruments,
            pid,
            no_attrs,
            &mut disks,
            &mut networks,
        );
    }
}

/// Single collection pass.
fn collect_once(
    sys: &mut System,
    instruments: &Instruments,
    pid: Pid,
    no_attrs: &[KeyValue],
    disks: &mut Option<Disks>,
    networks: &mut Option<Networks>,
) {
    sys.refresh_cpu_all();
    sys.refresh_memory();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    if let Some(gauge) = &instruments.system_cpu {
        let usage = f64::from(sys.global_cpu_usage()) / 100.0;
        gauge.record(usage, no_attrs);
    }

    if let Some(gauge) = &instruments.system_memory {
        let total = sys.total_memory();
        let available = sys.available_memory();
        if total > 0 {
            let utilization = 1.0 - (available as f64 / total as f64);
            gauge.record(utilization, no_attrs);
        }
    }

    if let Some(gauge) = &instruments.system_swap {
        let total = sys.total_swap();
        let used = sys.used_swap();
        if total > 0 {
            gauge.record(used as f64 / total as f64, no_attrs);
        }
    }

    if let Some(process) = sys.process(pid) {
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

    if let Some(d) = disks
        && let Some(gauge) = &instruments.disk_io
    {
        d.refresh(true);
        let (read, written) = d.iter().fold((0_u64, 0_u64), |(r, w), disk| {
            let usage = disk.usage();
            (r + usage.read_bytes, w + usage.written_bytes)
        });
        gauge.record(read as f64, &[KeyValue::new("direction", "read")]);
        gauge.record(written as f64, &[KeyValue::new("direction", "write")]);
    }

    if let Some(n) = networks
        && let Some(gauge) = &instruments.network_io
    {
        n.refresh(true);
        let (rx, tx) = n.iter().fold((0_u64, 0_u64), |(r, t), (_name, data)| {
            (r + data.total_received(), t + data.total_transmitted())
        });
        gauge.record(rx as f64, &[KeyValue::new("direction", "receive")]);
        gauge.record(tx as f64, &[KeyValue::new("direction", "transmit")]);
    }
}
