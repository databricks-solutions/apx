//! JSONL writer for Rust-side per-request bench traces.
//!
//! Gated behind `APX_BENCH_TRACE=1`. When enabled, each request through
//! `dispatch_traced()` appends a `RequestTrace` record to
//! `/tmp/bench_rust_trace.jsonl`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;

/// Path for Rust-side bench traces.
const TRACE_PATH: &str = "/tmp/bench_rust_trace.jsonl";

/// Module-level writer protected by Mutex. Lazily opened on first write.
static WRITER: Mutex<Option<BufWriter<File>>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// RequestTrace — per-request timing breakdown
// ---------------------------------------------------------------------------

/// Per-request timing breakdown from the Rust dispatch pipeline.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RequestTrace {
    /// Record discriminator for JSONL parsing.
    #[serde(rename = "type")]
    record_type: &'static str,
    path: String,
    method: String,
    status: u16,
    total_us: u64,
    body_collect_us: u64,
    gil_acquire_us: u64,
    scope_build_us: u64,
    app_call_us: u64,
    submit_us: u64,
    response_wait_us: u64,
}

// ---------------------------------------------------------------------------
// RequestTraceBuilder — accumulates per-phase timings
// ---------------------------------------------------------------------------

/// Accumulates per-phase timings during dispatch, producing a [`RequestTrace`].
#[derive(Debug)]
pub struct RequestTraceBuilder {
    method: String,
    path: String,
    body_collect_us: u64,
    gil_acquire_us: u64,
    scope_build_us: u64,
    app_call_us: u64,
    submit_us: u64,
    response_wait_us: u64,
}

impl RequestTraceBuilder {
    pub fn new(method: String, path: String) -> Self {
        Self {
            method,
            path,
            body_collect_us: 0,
            gil_acquire_us: 0,
            scope_build_us: 0,
            app_call_us: 0,
            submit_us: 0,
            response_wait_us: 0,
        }
    }

    pub fn body_collect(mut self, us: u64) -> Self {
        self.body_collect_us = us;
        self
    }

    pub fn gil_acquire(mut self, us: u64) -> Self {
        self.gil_acquire_us = us;
        self
    }

    pub fn scope_build(mut self, us: u64) -> Self {
        self.scope_build_us = us;
        self
    }

    pub fn app_call(mut self, us: u64) -> Self {
        self.app_call_us = us;
        self
    }

    pub fn submit(mut self, us: u64) -> Self {
        self.submit_us = us;
        self
    }

    pub fn response_wait(mut self, us: u64) -> Self {
        self.response_wait_us = us;
        self
    }

    pub fn build(self, total_us: u64, status: u16) -> RequestTrace {
        RequestTrace {
            record_type: "rust_req",
            path: self.path,
            method: self.method,
            status,
            total_us,
            body_collect_us: self.body_collect_us,
            gil_acquire_us: self.gil_acquire_us,
            scope_build_us: self.scope_build_us,
            app_call_us: self.app_call_us,
            submit_us: self.submit_us,
            response_wait_us: self.response_wait_us,
        }
    }
}

// ---------------------------------------------------------------------------
// File I/O — write / read / reset
// ---------------------------------------------------------------------------

/// Append a trace record. No-op if file open fails.
pub fn write(trace: &RequestTrace) {
    let Ok(mut guard) = WRITER.lock() else {
        return;
    };
    let writer = if let Some(w) = guard.as_mut() {
        w
    } else {
        let Ok(file) = File::options().append(true).create(true).open(TRACE_PATH) else {
            return;
        };
        guard.insert(BufWriter::new(file))
    };
    let _ = serde_json::to_writer(&mut *writer, trace);
    let _ = writer.write_all(b"\n");
    let _ = writer.flush();
}

/// Read all trace data. Returns `None` if file doesn't exist.
pub fn read() -> Option<String> {
    std::fs::read_to_string(TRACE_PATH).ok()
}

/// Close and delete the trace file.
pub fn reset() {
    if let Ok(mut guard) = WRITER.lock() {
        *guard = None;
    }
    let _ = std::fs::remove_file(TRACE_PATH);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_request_trace_serializes() {
        let trace = RequestTraceBuilder::new("GET".to_owned(), "/api/echo".to_owned())
            .body_collect(10)
            .gil_acquire(20)
            .scope_build(30)
            .app_call(40)
            .submit(50)
            .response_wait(60)
            .build(210, 200);

        let json = serde_json::to_string(&trace).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "rust_req");
        assert_eq!(parsed["method"], "GET");
        assert_eq!(parsed["path"], "/api/echo");
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["total_us"], 210);
        assert_eq!(parsed["body_collect_us"], 10);
        assert_eq!(parsed["submit_us"], 50);
    }
}
