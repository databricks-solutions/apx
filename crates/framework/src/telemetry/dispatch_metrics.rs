//! APX request pipeline metrics.
//!
//! Records per-phase latency via OTEL histograms and connection/request
//! counts via up-down counters. All instruments are lazily created on
//! first use and guarded by the `ApxMetricToggles` boolean flags —
//! disabled metrics have zero overhead.
//!
//! Toggles are initialized once per worker process via [`init`] after
//! reading the Python telemetry config.

use std::sync::OnceLock;

use opentelemetry::metrics::{Histogram, UpDownCounter};

use super::config::ApxMetricToggles;
use super::defs;
use super::http::framework_meter;

// ── Global toggles ────────────────────────────────────────────────────────

super::toggle_store!(TOGGLES: ApxMetricToggles = ApxMetricToggles {
    parse: false,
    scope_build: false,
    receive_build: false,
    send_parse: false,
    response_build: false,
    response_write: false,
    handler_wait: false,
    request_total: false,
    active_requests: false,
    connections: false,
});

// ── Metric declarations ───────────────────────────────────────────────────

const NO_ATTRS: &[opentelemetry::KeyValue] = &[];

/// Generate a lazy histogram getter and a gated public `record_*` function.
macro_rules! dispatch_metric {
    ($record_fn:ident, $hist_fn:ident, $toggle:ident, $def:expr, $doc:literal) => {
        fn $hist_fn() -> &'static Histogram<f64> {
            static INST: OnceLock<Histogram<f64>> = OnceLock::new();
            INST.get_or_init(|| $def.histogram(&framework_meter()))
        }

        #[doc = $doc]
        pub fn $record_fn(elapsed_us: f64) {
            if toggles().$toggle {
                $hist_fn().record(elapsed_us, NO_ATTRS);
            }
        }
    };
}

/// Generate a lazy up-down counter getter and gated `inc_*` / `dec_*` functions.
macro_rules! dispatch_gauge {
    ($inc_fn:ident, $dec_fn:ident, $counter_fn:ident, $toggle:ident, $def:expr, $doc:literal) => {
        fn $counter_fn() -> &'static UpDownCounter<i64> {
            static INST: OnceLock<UpDownCounter<i64>> = OnceLock::new();
            INST.get_or_init(|| $def.up_down_counter(&framework_meter()))
        }

        #[doc = $doc]
        pub fn $inc_fn() {
            if toggles().$toggle {
                $counter_fn().add(1, NO_ATTRS);
            }
        }

        /// Decrement the counter.
        pub fn $dec_fn() {
            if toggles().$toggle {
                $counter_fn().add(-1, NO_ATTRS);
            }
        }
    };
}

// ── Histograms ───────────────────────────────────────────────────────────

dispatch_metric!(
    record_parse,
    parse_hist,
    parse,
    defs::PARSE,
    "Record `apx.parse` if enabled."
);

dispatch_metric!(
    record_scope_build,
    scope_build_hist,
    scope_build,
    defs::SCOPE_BUILD,
    "Record `apx.scope_build` if enabled."
);

dispatch_metric!(
    record_receive_build,
    receive_build_hist,
    receive_build,
    defs::RECEIVE_BUILD,
    "Record `apx.receive_build` if enabled."
);

dispatch_metric!(
    record_send_parse,
    send_parse_hist,
    send_parse,
    defs::SEND_PARSE,
    "Record `apx.send_parse` if enabled."
);

dispatch_metric!(
    record_response_build,
    response_build_hist,
    response_build,
    defs::RESPONSE_BUILD,
    "Record `apx.response_build` if enabled."
);

dispatch_metric!(
    record_response_write,
    response_write_hist,
    response_write,
    defs::RESPONSE_WRITE,
    "Record `apx.response_write` if enabled."
);

dispatch_metric!(
    record_handler_wait,
    handler_wait_hist,
    handler_wait,
    defs::HANDLER_WAIT,
    "Record `apx.handler_wait` if enabled."
);

dispatch_metric!(
    record_dispatch_total,
    dispatch_total_hist,
    request_total,
    defs::REQUEST_TOTAL,
    "Record `apx.request_total` if enabled."
);

// ── Up-down counters ─────────────────────────────────────────────────────

dispatch_gauge!(
    inc_active_requests,
    dec_active_requests,
    active_requests_gauge,
    active_requests,
    defs::ACTIVE_REQUESTS,
    "Increment `apx.active_requests`."
);

dispatch_gauge!(
    inc_connections,
    dec_connections,
    connections_gauge,
    connections,
    defs::CONNECTIONS,
    "Increment `apx.connections`."
);
