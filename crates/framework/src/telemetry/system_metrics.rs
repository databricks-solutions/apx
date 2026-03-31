//! Machine-wide system metrics via OTEL observable gauges.
//!
//! Registers per-metric observable instruments whose callbacks are invoked by
//! the SDK at each export cycle. A shared [`SystemState`] wraps `sysinfo`
//! types behind `Arc<Mutex<_>>` with a staleness guard so that multiple
//! callbacks in the same export cycle share a single refresh.
//!
//! Collected once on the supervisor process only.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentelemetry::KeyValue;
use opentelemetry::metrics::AsyncInstrument;
use sysinfo::{Disks, Networks, System};

use super::Refreshable;
use super::config::SystemGlobalToggles;
use super::defs;
use super::http::framework_meter;

/// Minimum elapsed time before `sysinfo` data is re-read.
const STALENESS_THRESHOLD: Duration = Duration::from_secs(1);

/// Shared mutable state for all system-level observable callbacks.
struct SystemState {
    sys: System,
    disks: Option<Disks>,
    networks: Option<Networks>,
    last_refresh: Instant,
}

impl SystemState {
    fn new(toggles: SystemGlobalToggles) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();

        Self {
            sys,
            disks: toggles.system_disk_io.then(Disks::new),
            networks: toggles.system_network_io.then(Networks::new),
            last_refresh: Instant::now(),
        }
    }
}

impl Refreshable for SystemState {
    fn ensure_fresh(&mut self) {
        if self.last_refresh.elapsed() < STALENESS_THRESHOLD {
            return;
        }
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        if let Some(d) = &mut self.disks {
            d.refresh(true);
        }
        if let Some(n) = &mut self.networks {
            n.refresh(true);
        }
        self.last_refresh = Instant::now();
    }
}

/// Register observable gauges for each enabled system metric.
///
/// Callbacks are owned by the meter provider; no handle needs to be stored.
pub fn register_system_metrics(toggles: SystemGlobalToggles) {
    let meter = framework_meter();
    let state = Arc::new(Mutex::new(SystemState::new(toggles)));

    if toggles.system_cpu {
        let s = Arc::clone(&state);
        let _gauge = defs::SYSTEM_CPU.observable_gauge(&meter, move |i| observe_cpu(&s, i));
    }
    if toggles.system_memory {
        let s = Arc::clone(&state);
        let _gauge = defs::SYSTEM_MEMORY.observable_gauge(&meter, move |i| observe_memory(&s, i));
    }
    if toggles.system_paging {
        let s = Arc::clone(&state);
        let _gauge = defs::SYSTEM_PAGING.observable_gauge(&meter, move |i| observe_paging(&s, i));
    }
    if toggles.system_disk_io {
        let s = Arc::clone(&state);
        let _gauge = defs::SYSTEM_DISK_IO.observable_gauge(&meter, move |i| observe_disk_io(&s, i));
    }
    if toggles.system_network_io {
        let s = Arc::clone(&state);
        let _gauge =
            defs::SYSTEM_NETWORK_IO.observable_gauge(&meter, move |i| observe_network_io(&s, i));
    }

    tracing::trace!(
        name: "apx.telemetry.system_metrics.registered",
        target: "apx::telemetry",
        "system observable gauges registered"
    );
}

// ── Per-metric observe callbacks ─────────────────────────────────────────

fn observe_cpu(state: &Arc<Mutex<SystemState>>, instrument: &dyn AsyncInstrument<f64>) {
    super::with_fresh(state, |s| {
        let usage = f64::from(s.sys.global_cpu_usage()) / 100.0;
        instrument.observe(usage, &[]);
    });
}

fn observe_memory(state: &Arc<Mutex<SystemState>>, instrument: &dyn AsyncInstrument<f64>) {
    super::with_fresh(state, |s| {
        let total = s.sys.total_memory();
        let available = s.sys.available_memory();
        if total > 0 {
            instrument.observe(1.0 - (available as f64 / total as f64), &[]);
        }
    });
}

fn observe_paging(state: &Arc<Mutex<SystemState>>, instrument: &dyn AsyncInstrument<f64>) {
    super::with_fresh(state, |s| {
        let total = s.sys.total_swap();
        let used = s.sys.used_swap();
        if total > 0 {
            instrument.observe(used as f64 / total as f64, &[]);
        }
    });
}

fn observe_disk_io(state: &Arc<Mutex<SystemState>>, instrument: &dyn AsyncInstrument<f64>) {
    super::with_fresh(state, |s| {
        let Some(disks) = &s.disks else { return };
        let (read, written) = disks.iter().fold((0_u64, 0_u64), |(r, w), disk| {
            let usage = disk.usage();
            (r + usage.read_bytes, w + usage.written_bytes)
        });
        instrument.observe(read as f64, &[KeyValue::new("disk.io.direction", "read")]);
        instrument.observe(
            written as f64,
            &[KeyValue::new("disk.io.direction", "write")],
        );
    });
}

fn observe_network_io(state: &Arc<Mutex<SystemState>>, instrument: &dyn AsyncInstrument<f64>) {
    super::with_fresh(state, |s| {
        let Some(networks) = &s.networks else { return };
        let (rx, tx) = networks
            .iter()
            .fold((0_u64, 0_u64), |(r, t), (_name, data)| {
                (r + data.total_received(), t + data.total_transmitted())
            });
        instrument.observe(
            rx as f64,
            &[KeyValue::new("network.io.direction", "receive")],
        );
        instrument.observe(
            tx as f64,
            &[KeyValue::new("network.io.direction", "transmit")],
        );
    });
}
