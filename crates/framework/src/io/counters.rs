//! Lightweight atomic counters for request telemetry.
//!
//! All counters use `Relaxed` ordering — they are monotonic and read only
//! for periodic reporting, not synchronization.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

// ---------------------------------------------------------------------------
// RequestCounters
// ---------------------------------------------------------------------------

/// Aggregate request metrics across all requests in a worker.
#[derive(Debug)]
pub struct RequestCounters {
    requests_total: AtomicU64,
    requests_errors: AtomicU64,
    requests_in_flight: AtomicUsize,
}

impl RequestCounters {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_errors: AtomicU64::new(0),
            requests_in_flight: AtomicUsize::new(0),
        }
    }

    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            requests_errors: self.requests_errors.load(Ordering::Relaxed),
            requests_in_flight: self.requests_in_flight.load(Ordering::Relaxed) as u64,
        }
    }
}

// ---------------------------------------------------------------------------
// CounterSnapshot — serializable point-in-time read
// ---------------------------------------------------------------------------

/// Serializable snapshot of request counters.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CounterSnapshot {
    pub requests_total: u64,
    pub requests_errors: u64,
    pub requests_in_flight: u64,
}

// ---------------------------------------------------------------------------
// Global access via OnceLock
// ---------------------------------------------------------------------------

/// Per-worker request counters, set during `EventLoop::init()`.
static COUNTERS: OnceLock<Arc<RequestCounters>> = OnceLock::new();

pub fn init(counters: Arc<RequestCounters>) {
    let _ = COUNTERS.set(counters);
}

pub fn get() -> Option<&'static Arc<RequestCounters>> {
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
    fn test_counter_snapshot_serializes() {
        let counters = RequestCounters::new();
        let snap = counters.snapshot();
        assert_eq!(snap.requests_total, 0);
        assert_eq!(snap.requests_errors, 0);
        assert_eq!(snap.requests_in_flight, 0);

        let json = serde_json::to_string(&snap).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["requests_total"], 0);
        assert_eq!(parsed["requests_errors"], 0);
    }
}
