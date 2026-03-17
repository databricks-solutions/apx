//! Lightweight atomic counters for scheduler instrumentation.
//!
//! All counters use `Relaxed` ordering — they are monotonic and read only
//! for periodic reporting, not synchronization.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use crate::io::driver::DriveStats;

// ---------------------------------------------------------------------------
// SchedulerCounters
// ---------------------------------------------------------------------------

/// Aggregate scheduler metrics across all requests in a worker.
///
/// All counters use `Relaxed` ordering — they are monotonic and
/// read only for periodic reporting, not synchronization.
#[derive(Debug)]
pub struct SchedulerCounters {
    tasks_spawned: AtomicU64,
    inline_completions: AtomicU64,
    suspensions: AtomicU64,
    budget_exhaustions: AtomicU64,
    yield_none_total: AtomicU64,
    yield_future_total: AtomicU64,
    yield_asyncio_future_total: AtomicU64,
    yield_coroutine_total: AtomicU64,
    drive_steps_total: AtomicU64,
    peak_queue_depth: AtomicUsize,
    current_queue_depth: AtomicUsize,
}

impl SchedulerCounters {
    pub fn new() -> Self {
        Self {
            tasks_spawned: AtomicU64::new(0),
            inline_completions: AtomicU64::new(0),
            suspensions: AtomicU64::new(0),
            budget_exhaustions: AtomicU64::new(0),
            yield_none_total: AtomicU64::new(0),
            yield_future_total: AtomicU64::new(0),
            yield_asyncio_future_total: AtomicU64::new(0),
            yield_coroutine_total: AtomicU64::new(0),
            drive_steps_total: AtomicU64::new(0),
            peak_queue_depth: AtomicUsize::new(0),
            current_queue_depth: AtomicUsize::new(0),
        }
    }

    pub fn record_spawn(&self) {
        self.tasks_spawned.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inline_completion(&self) {
        self.inline_completions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_suspension(&self) {
        self.suspensions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_budget_exhaustion(&self) {
        self.budget_exhaustions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_drive(&self, stats: &DriveStats) {
        self.drive_steps_total
            .fetch_add(u64::from(stats.steps), Ordering::Relaxed);
        self.yield_none_total
            .fetch_add(u64::from(stats.yield_none), Ordering::Relaxed);
        self.yield_future_total
            .fetch_add(u64::from(stats.yield_future), Ordering::Relaxed);
        self.yield_asyncio_future_total
            .fetch_add(u64::from(stats.yield_asyncio_future), Ordering::Relaxed);
        self.yield_coroutine_total
            .fetch_add(u64::from(stats.yield_coroutine), Ordering::Relaxed);
    }

    pub fn record_enqueue(&self) {
        let prev = self.current_queue_depth.fetch_add(1, Ordering::Relaxed);
        let new_depth = prev + 1;
        self.peak_queue_depth
            .fetch_max(new_depth, Ordering::Relaxed);
    }

    pub fn record_dequeue(&self) {
        self.current_queue_depth.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            tasks_spawned: self.tasks_spawned.load(Ordering::Relaxed),
            inline_completions: self.inline_completions.load(Ordering::Relaxed),
            suspensions: self.suspensions.load(Ordering::Relaxed),
            budget_exhaustions: self.budget_exhaustions.load(Ordering::Relaxed),
            yield_none_total: self.yield_none_total.load(Ordering::Relaxed),
            yield_future_total: self.yield_future_total.load(Ordering::Relaxed),
            yield_asyncio_future_total: self.yield_asyncio_future_total.load(Ordering::Relaxed),
            yield_coroutine_total: self.yield_coroutine_total.load(Ordering::Relaxed),
            drive_steps_total: self.drive_steps_total.load(Ordering::Relaxed),
            peak_queue_depth: self.peak_queue_depth.load(Ordering::Relaxed) as u64,
            current_queue_depth: self.current_queue_depth.load(Ordering::Relaxed) as u64,
        }
    }
}

// ---------------------------------------------------------------------------
// CounterSnapshot — serializable point-in-time read
// ---------------------------------------------------------------------------

/// Serializable snapshot of scheduler counters.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CounterSnapshot {
    pub tasks_spawned: u64,
    pub inline_completions: u64,
    pub suspensions: u64,
    pub budget_exhaustions: u64,
    pub yield_none_total: u64,
    pub yield_future_total: u64,
    pub yield_asyncio_future_total: u64,
    pub yield_coroutine_total: u64,
    pub drive_steps_total: u64,
    pub peak_queue_depth: u64,
    pub current_queue_depth: u64,
}

// ---------------------------------------------------------------------------
// Global access via OnceLock
// ---------------------------------------------------------------------------

/// Per-worker scheduler counters, set during `EventLoop::init()`.
static COUNTERS: OnceLock<Arc<SchedulerCounters>> = OnceLock::new();

pub fn init(counters: Arc<SchedulerCounters>) {
    let _ = COUNTERS.set(counters);
}

pub fn get() -> Option<&'static Arc<SchedulerCounters>> {
    COUNTERS.get()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_drive_stats_default_is_zero() {
        let stats = DriveStats::default();
        assert_eq!(stats.steps, 0);
        assert_eq!(stats.yield_none, 0);
        assert_eq!(stats.yield_future, 0);
        assert_eq!(stats.yield_asyncio_future, 0);
        assert_eq!(stats.yield_coroutine, 0);
        assert_eq!(stats.yield_unknown, 0);
        assert!(!stats.budget_exhausted);
    }

    #[test]
    fn test_counter_snapshot_roundtrip() {
        let counters = SchedulerCounters::new();
        counters.record_spawn();
        counters.record_spawn();
        counters.record_inline_completion();

        let stats = DriveStats {
            steps: 10,
            yield_none: 5,
            yield_future: 2,
            ..DriveStats::default()
        };
        counters.record_drive(&stats);

        let snap = counters.snapshot();
        assert_eq!(snap.tasks_spawned, 2);
        assert_eq!(snap.inline_completions, 1);
        assert_eq!(snap.drive_steps_total, 10);
        assert_eq!(snap.yield_none_total, 5);
        assert_eq!(snap.yield_future_total, 2);

        let json = serde_json::to_string(&snap).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["tasks_spawned"], 2);
    }
}
