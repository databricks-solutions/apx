//! Per-process metrics via OTEL observable gauges.
//!
//! Registers per-metric observable instruments whose callbacks are invoked by
//! the SDK at each export cycle. A shared [`ProcessState`] wraps `sysinfo`
//! types behind `Arc<Mutex<_>>` with a staleness guard so that multiple
//! callbacks in the same export cycle share a single refresh.
//!
//! Collected per-worker and once for the supervisor.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentelemetry::metrics::AsyncInstrument;
use sysinfo::{Pid, System};

use super::Refreshable;
use super::config::ProcessMetricToggles;
use super::defs;
use super::http::framework_meter;

/// Minimum elapsed time before `sysinfo` data is re-read.
const STALENESS_THRESHOLD: Duration = Duration::from_secs(1);

/// Shared mutable state for all process-level observable callbacks.
struct ProcessState {
    sys: System,
    pid: Pid,
    last_refresh: Instant,
}

impl ProcessState {
    fn new(pid: Pid) -> Self {
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

        Self {
            sys,
            pid,
            last_refresh: Instant::now(),
        }
    }
}

impl Refreshable for ProcessState {
    fn ensure_fresh(&mut self) {
        if self.last_refresh.elapsed() < STALENESS_THRESHOLD {
            return;
        }
        self.sys
            .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[self.pid]), true);
        self.last_refresh = Instant::now();
    }
}

/// Register observable gauges for each enabled process metric.
///
/// Callbacks are owned by the meter provider; no handle needs to be stored.
pub fn register_process_metrics(toggles: ProcessMetricToggles) {
    let meter = framework_meter();
    let pid = Pid::from_u32(std::process::id());
    let state = Arc::new(Mutex::new(ProcessState::new(pid)));

    if toggles.process_cpu {
        let s = Arc::clone(&state);
        let _gauge =
            defs::PROCESS_CPU.observable_gauge(&meter, move |i| observe_process_cpu(&s, i));
    }
    if toggles.process_memory {
        let s = Arc::clone(&state);
        let _gauge =
            defs::PROCESS_MEMORY.observable_gauge(&meter, move |i| observe_process_memory(&s, i));
    }
    if toggles.process_threads {
        let s = Arc::clone(&state);
        let _gauge =
            defs::PROCESS_THREADS.observable_gauge(&meter, move |i| observe_process_threads(&s, i));
    }

    tracing::trace!(
        name: "apx.telemetry.process_metrics.registered",
        target: "apx::telemetry",
        pid = pid.as_u32(),
        "process observable gauges registered"
    );
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Refresh state and look up the tracked process, invoking `f` if found.
fn with_process<F>(state: &Arc<Mutex<ProcessState>>, f: F)
where
    F: FnOnce(&sysinfo::Process),
{
    super::with_fresh(state, |s| {
        if let Some(process) = s.sys.process(s.pid) {
            f(process);
        }
    });
}

// ── Per-metric observe callbacks ─────────────────────────────────────────

fn observe_process_cpu(state: &Arc<Mutex<ProcessState>>, instrument: &dyn AsyncInstrument<f64>) {
    with_process(state, |p| {
        let usage = f64::from(p.cpu_usage()) / 100.0;
        instrument.observe(usage, &[]);
    });
}

fn observe_process_memory(state: &Arc<Mutex<ProcessState>>, instrument: &dyn AsyncInstrument<f64>) {
    with_process(state, |p| {
        instrument.observe(p.memory() as f64, &[]);
    });
}

fn observe_process_threads(
    state: &Arc<Mutex<ProcessState>>,
    instrument: &dyn AsyncInstrument<f64>,
) {
    with_process(state, |p| {
        if let Some(tasks) = p.tasks() {
            instrument.observe(tasks.len() as f64, &[]);
        }
    });
}
