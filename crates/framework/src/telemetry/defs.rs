//! Metric definitions — single source of truth for names, descriptions, and units.
//!
//! Every framework metric is declared as a [`MetricDef`] constant. Instrument
//! creation, config doc comments, and Python toggle models all reference these
//! constants instead of duplicating string literals.

use opentelemetry::metrics::{AsyncInstrument, Gauge, Histogram, Meter, ObservableGauge};

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

// ── APX dispatch metrics (per-worker) ────────────────────────────────────

/// Time to collect the request body from the network stream.
pub const DISPATCH_BODY_COLLECT: MetricDef = MetricDef {
    name: "apx.dispatch.body_collect.duration",
    description: "Time to collect the request body from the network stream",
    unit: "us",
};

/// Time to push the request slot to the crossbeam channel and signal wakeup.
pub const DISPATCH_CROSSBEAM_SEND: MetricDef = MetricDef {
    name: "apx.dispatch.crossbeam_send.duration",
    description: "Time to push the request slot to the crossbeam channel and signal wakeup",
    unit: "us",
};

/// Time waiting for the Python handler to produce a response.
pub const DISPATCH_RESPONSE_WAIT: MetricDef = MetricDef {
    name: "apx.dispatch.response_wait.duration",
    description: "Time waiting for the Python handler to produce a response",
    unit: "us",
};

/// Total dispatch duration from body collect start to response ready.
pub const DISPATCH_TOTAL: MetricDef = MetricDef {
    name: "apx.dispatch.total.duration",
    description: "Total dispatch duration from body collect start to response ready",
    unit: "us",
};

/// Time to build the ASGI receive dict for the Python handler.
pub const ASGI_RECEIVE_BUILD: MetricDef = MetricDef {
    name: "apx.asgi.receive_build.duration",
    description: "Time to build the ASGI receive dict for the Python handler",
    unit: "us",
};

/// Time to parse the ASGI send event dict from the Python handler.
pub const ASGI_SEND_PARSE: MetricDef = MetricDef {
    name: "apx.asgi.send_parse.duration",
    description: "Time to parse the ASGI send event dict from the Python handler",
    unit: "us",
};

/// Time from slot creation on the tokio thread to pickup on the asyncio thread.
pub const DISPATCH_PICKUP_DELAY: MetricDef = MetricDef {
    name: "apx.dispatch.pickup_delay.duration",
    description: "Time from slot creation to asyncio thread pickup",
    unit: "us",
};

/// Time to build the ASGI scope dict and receive/send callables.
pub const DISPATCH_MATERIALIZE: MetricDef = MetricDef {
    name: "apx.dispatch.materialize.duration",
    description: "Time to build ASGI scope and receive/send callables",
    unit: "us",
};

/// Number of pending request slots in the crossbeam channel at drain time.
pub const DISPATCH_QUEUE_DEPTH: MetricDef = MetricDef {
    name: "apx.dispatch.queue_depth",
    description: "Pending request slots in the crossbeam channel at drain time",
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
    // APX dispatch (per-worker)
    MetricCatalogEntry {
        def: DISPATCH_BODY_COLLECT,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_CROSSBEAM_SEND,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_RESPONSE_WAIT,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_TOTAL,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: ASGI_RECEIVE_BUILD,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: ASGI_SEND_PARSE,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_PICKUP_DELAY,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_MATERIALIZE,
        group: "apx",
        scope: "worker",
    },
    MetricCatalogEntry {
        def: DISPATCH_QUEUE_DEPTH,
        group: "apx",
        scope: "worker",
    },
];
