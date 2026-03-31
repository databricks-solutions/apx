//! Machine-wide system metrics collection via `sysinfo`.
//!
//! Spawns a tokio task that periodically reads CPU, memory, swap, disk,
//! and network metrics and reports them through the OTEL metrics pipeline.
//! Collected once on the supervisor process only.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Gauge;
use sysinfo::{Disks, Networks, System};

use super::config::SystemConfig;
use super::defs;
use super::http::framework_meter;

/// Spawn the system metrics collection background task.
///
/// Returns the `JoinHandle` so the caller can abort on shutdown.
pub fn spawn_system_metrics(config: &SystemConfig) -> tokio::task::JoinHandle<()> {
    let toggles = config.metrics;
    let interval = Duration::from_secs_f64(config.interval_secs);

    tracing::trace!(
        name: "apx.telemetry.system_metrics.started",
        target: "apx::telemetry",
        interval_secs = config.interval_secs,
        "spawning system metrics collection task"
    );

    tokio::spawn(async move {
        collection_loop(toggles, interval).await;
    })
}

/// System-global instruments, created once and reused.
struct Instruments {
    system_cpu: Option<Gauge<f64>>,
    system_memory: Option<Gauge<f64>>,
    system_swap: Option<Gauge<f64>>,
    disk_io: Option<Gauge<f64>>,
    network_io: Option<Gauge<f64>>,
}

impl Instruments {
    fn new(toggles: super::config::SystemGlobalToggles) -> Self {
        let meter = framework_meter();
        Self {
            system_cpu: defs::SYSTEM_CPU.optional_gauge(&meter, toggles.system_cpu),
            system_memory: defs::SYSTEM_MEMORY.optional_gauge(&meter, toggles.system_memory),
            system_swap: defs::SYSTEM_SWAP.optional_gauge(&meter, toggles.system_swap),
            disk_io: defs::SYSTEM_DISK_IO.optional_gauge(&meter, toggles.system_disk_io),
            network_io: defs::SYSTEM_NETWORK_IO.optional_gauge(&meter, toggles.system_network_io),
        }
    }
}

/// Periodic collection loop.
async fn collection_loop(toggles: super::config::SystemGlobalToggles, interval: Duration) {
    let instruments = Instruments::new(toggles);
    let mut sys = System::new();
    let mut disks = toggles.system_disk_io.then(Disks::new);
    let mut networks = toggles.system_network_io.then(Networks::new);
    let no_attrs: &[KeyValue] = &[];
    let mut first_tick = true;

    loop {
        tokio::time::sleep(interval).await;
        collect_once(&mut sys, &instruments, no_attrs, &mut disks, &mut networks);

        if first_tick {
            sys.refresh_cpu_all();
            sys.refresh_memory();
            let cpu_pct = sys.global_cpu_usage();
            let mem_used_mb = (sys.total_memory() - sys.available_memory()) / (1024 * 1024);
            tracing::debug!(
                name: "apx.telemetry.system_metrics.first_collection",
                target: "apx::telemetry",
                interval_ms = interval.as_millis(),
                cpu_pct = format_args!("{cpu_pct:.1}"),
                mem_used_mb,
                "system metrics: first collection recorded"
            );
            first_tick = false;
        }
    }
}

/// Single collection pass.
fn collect_once(
    sys: &mut System,
    instruments: &Instruments,
    no_attrs: &[KeyValue],
    disks: &mut Option<Disks>,
    networks: &mut Option<Networks>,
) {
    sys.refresh_cpu_all();
    sys.refresh_memory();

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
