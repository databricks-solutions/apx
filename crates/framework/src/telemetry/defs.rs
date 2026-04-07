//! Metric definitions — single source of truth for names, descriptions, and units.
//!
//! Every framework metric is declared as a [`MetricDef`] constant. Instrument
//! creation, config doc comments, and Python toggle models all reference these
//! constants instead of duplicating string literals.

use opentelemetry::metrics::{
    AsyncInstrument, Gauge, Histogram, Meter, ObservableGauge, UpDownCounter,
};

/// Descriptor for an OTEL metric instrument.
#[derive(Debug, Clone, Copy)]
pub struct MetricDef {
    /// OTEL metric name (e.g. `"process.cpu.utilization"`).
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// UCUM unit string.
    pub unit: &'static str,
}

impl MetricDef {
    /// Build an f64 gauge from this definition.
    pub fn gauge(self, meter: &Meter) -> Gauge<f64> {
        meter
            .f64_gauge(self.name)
            .with_description(self.description)
            .with_unit(self.unit)
            .build()
    }

    /// Build an f64 gauge only if the toggle is enabled.
    pub fn optional_gauge(self, meter: &Meter, enabled: bool) -> Option<Gauge<f64>> {
        enabled.then(|| self.gauge(meter))
    }

    /// Build an f64 histogram from this definition.
    pub fn histogram(self, meter: &Meter) -> Histogram<f64> {
        meter
            .f64_histogram(self.name)
            .with_description(self.description)
            .with_unit(self.unit)
            .build()
    }

    /// Build an i64 up-down counter from this definition.
    pub fn up_down_counter(self, meter: &Meter) -> UpDownCounter<i64> {
        meter
            .i64_up_down_counter(self.name)
            .with_description(self.description)
            .with_unit(self.unit)
            .build()
    }

    /// Build an observable f64 gauge that reports via a callback.
    pub fn observable_gauge<F>(self, meter: &Meter, callback: F) -> ObservableGauge<f64>
    where
        F: Fn(&dyn AsyncInstrument<f64>) + Send + Sync + 'static,
    {
        meter
            .f64_observable_gauge(self.name)
            .with_description(self.description)
            .with_unit(self.unit)
            .with_callback(callback)
            .build()
    }
}

// ── System-global metrics (supervisor only) ──────────────────────────────

/// System-wide CPU utilization as a fraction (supervisor only).
pub const SYSTEM_CPU: MetricDef = MetricDef {
    name: "system.cpu.utilization",
    description: "System-wide CPU utilization as a fraction",
    unit: "1",
};

/// Fraction of available memory used (supervisor only).
pub const SYSTEM_MEMORY: MetricDef = MetricDef {
    name: "system.memory.utilization",
    description: "Fraction of available memory used",
    unit: "1",
};

/// Fraction of paging (swap) space used (supervisor only).
pub const SYSTEM_PAGING: MetricDef = MetricDef {
    name: "system.paging.utilization",
    description: "Fraction of paging (swap) space used",
    unit: "1",
};

/// Cumulative disk I/O in bytes (supervisor only).
pub const SYSTEM_DISK_IO: MetricDef = MetricDef {
    name: "system.disk.io",
    description: "Cumulative disk I/O in bytes",
    unit: "By",
};

/// Cumulative network I/O in bytes (supervisor only).
pub const SYSTEM_NETWORK_IO: MetricDef = MetricDef {
    name: "system.network.io",
    description: "Cumulative network I/O in bytes",
    unit: "By",
};

// ── Process metrics (per-worker + supervisor) ────────────────────────────

/// Process CPU utilization as a fraction of one core.
pub const PROCESS_CPU: MetricDef = MetricDef {
    name: "process.cpu.utilization",
    description: "Process CPU utilization as a fraction of one core",
    unit: "1",
};

/// Process resident memory in bytes.
pub const PROCESS_MEMORY: MetricDef = MetricDef {
    name: "process.memory.usage",
    description: "Process resident memory in bytes",
    unit: "By",
};

/// Number of threads in the process.
pub const PROCESS_THREADS: MetricDef = MetricDef {
    name: "process.thread.count",
    description: "Number of threads in the process",
    unit: "1",
};

// ── HTTP metrics (per-worker) ────────────────────────────────────────────

/// Duration of HTTP server requests.
pub const HTTP_REQUEST_DURATION: MetricDef = MetricDef {
    name: "http.server.request.duration",
    description: "Duration of HTTP server requests",
    unit: "s",
};

/// Number of in-flight HTTP server requests.
pub const HTTP_ACTIVE_REQUESTS: MetricDef = MetricDef {
    name: "http.server.active_requests",
    description: "Number of in-flight HTTP server requests",
    unit: "1",
};

// ── APX protocol metrics (per-worker) ─────────────────────────────────

/// HTTP request parsing time.
pub const PARSE: MetricDef = MetricDef {
    name: "apx.parse",
    description: "HTTP request parsing time",
    unit: "us",
};

/// ASGI scope dict construction time.
pub const SCOPE_BUILD: MetricDef = MetricDef {
    name: "apx.scope_build",
    description: "ASGI scope dict construction time",
    unit: "us",
};

/// ASGI receive dict construction time.
pub const RECEIVE_BUILD: MetricDef = MetricDef {
    name: "apx.receive_build",
    description: "ASGI receive dict construction time",
    unit: "us",
};

/// ASGI send event parsing time.
pub const SEND_PARSE: MetricDef = MetricDef {
    name: "apx.send_parse",
    description: "ASGI send event parsing time",
    unit: "us",
};

/// HTTP response header construction time.
pub const RESPONSE_BUILD: MetricDef = MetricDef {
    name: "apx.response_build",
    description: "HTTP response header construction time",
    unit: "us",
};

/// Transport write time.
pub const RESPONSE_WRITE: MetricDef = MetricDef {
    name: "apx.response_write",
    description: "Transport write time",
    unit: "us",
};

/// Handler execution time (dispatch to response complete).
pub const HANDLER_WAIT: MetricDef = MetricDef {
    name: "apx.handler_wait",
    description: "Handler execution time",
    unit: "us",
};

/// Total request processing time.
pub const REQUEST_TOTAL: MetricDef = MetricDef {
    name: "apx.request_total",
    description: "Total request processing time",
    unit: "us",
};

/// In-flight requests on this worker.
pub const ACTIVE_REQUESTS: MetricDef = MetricDef {
    name: "apx.active_requests",
    description: "In-flight requests on this worker",
    unit: "1",
};

/// Active TCP connections on this worker.
pub const CONNECTIONS: MetricDef = MetricDef {
    name: "apx.connections",
    description: "Active TCP connections on this worker",
    unit: "1",
};

// ── Catalog ──────────────────────────────────────────────────────────────

/// A metric definition with additional classification metadata.
#[derive(Debug, Clone, Copy)]
pub struct MetricCatalogEntry {
    /// The core metric definition (name, description, unit).
    pub def: MetricDef,
    /// Logical group: `"system"`, `"process"`, `"http"`, or `"apx"`.
    pub group: &'static str,
    /// Collection scope: `"supervisor"`, `"worker"`, or `"both"`.
    pub scope: &'static str,
}

/// Complete catalog of all framework-defined metrics.
pub static ALL_METRICS: &[MetricCatalogEntry] = &[
    // System-global (supervisor only)
    MetricCatalogEntry {
        def: SYSTEM_CPU,
        group: "system",
        scope: "supervisor",
    },
    MetricCatalogEntry {
        def: SYSTEM_MEMORY,
        group: "system",
        scope: "supervisor",
    },
    MetricCatalogEntry {
        def: SYSTEM_PAGING,
        group: "system",
        scope: "supervisor",
    },
    MetricCatalogEntry {
        def: SYSTEM_DISK_IO,
        group: "system",
        scope: "supervisor",
    },
    MetricCatalogEntry {
        def: SYSTEM_NETWORK_IO,
        group: "system",
        scope: "supervisor",
    },
    // Process (both supervisor and workers)
    MetricCatalogEntry {
        def: PROCESS_CPU,
        group: "process",
        scope: "both",
    },
    MetricCatalogEntry {
        def: PROCESS_MEMORY,
        group: "process",
        scope: "both",
    },
    MetricCatalogEntry {
        def: PROCESS_THREADS,
        group: "process",
        scope: "both",
    },
    // HTTP (per-worker)
    MetricCatalogEntry {
        def: HTTP_REQUEST_DURATION,
        group: "http",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: HTTP_ACTIVE_REQUESTS,
        group: "http",
        scope: "worker",
    },
    // APX request pipeline (per-worker)
    MetricCatalogEntry {
        def: PARSE,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: SCOPE_BUILD,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: RECEIVE_BUILD,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: SEND_PARSE,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: RESPONSE_BUILD,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: RESPONSE_WRITE,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: HANDLER_WAIT,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: REQUEST_TOTAL,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: ACTIVE_REQUESTS,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: CONNECTIONS,
        group: "apx",
        scope: "worker",
    },
];
