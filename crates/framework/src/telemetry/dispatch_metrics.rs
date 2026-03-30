//! APX framework dispatch timing histograms.
//!
//! Records per-phase latency for the ASGI dispatch pipeline via OTEL
//! histograms. All instruments are lazily created on first use and guarded
//! by the `ApxMetricToggles` boolean flags — disabled metrics have zero
//! overhead.
//!
//! Toggles are initialized once per worker process via [`init`] after
//! reading the Python telemetry config.

use std::sync::OnceLock;

use opentelemetry::metrics::Histogram;

use super::config::ApxMetricToggles;
use super::defs;
use super::http::framework_meter;

// ── Global toggles ────────────────────────────────────────────────────────

super::toggle_store!(TOGGLES: ApxMetricToggles = ApxMetricToggles {
    dispatch_body_collect: false,
    dispatch_crossbeam_send: false,
    dispatch_response_wait: false,
    dispatch_total: false,
    asgi_receive_build: false,
    asgi_send_parse: false,
    dispatch_pickup_delay: false,
    dispatch_materialize: false,
    dispatch_queue_depth: false,
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

dispatch_metric!(
    record_body_collect,
    body_collect_hist,
    dispatch_body_collect,
    defs::DISPATCH_BODY_COLLECT,
    "Record `apx.dispatch.body_collect.duration` if enabled."
);
dispatch_metric!(
    record_crossbeam_send,
    crossbeam_send_hist,
    dispatch_crossbeam_send,
    defs::DISPATCH_CROSSBEAM_SEND,
    "Record `apx.dispatch.crossbeam_send.duration` if enabled."
);
dispatch_metric!(
    record_response_wait,
    response_wait_hist,
    dispatch_response_wait,
    defs::DISPATCH_RESPONSE_WAIT,
    "Record `apx.dispatch.response_wait.duration` if enabled."
);
dispatch_metric!(
    record_dispatch_total,
    dispatch_total_hist,
    dispatch_total,
    defs::DISPATCH_TOTAL,
    "Record `apx.dispatch.total.duration` if enabled."
);
dispatch_metric!(
    record_receive_build,
    receive_build_hist,
    asgi_receive_build,
    defs::ASGI_RECEIVE_BUILD,
    "Record `apx.asgi.receive_build.duration` if enabled."
);
dispatch_metric!(
    record_send_parse,
    send_parse_hist,
    asgi_send_parse,
    defs::ASGI_SEND_PARSE,
    "Record `apx.asgi.send_parse.duration` if enabled."
);
dispatch_metric!(
    record_pickup_delay,
    pickup_delay_hist,
    dispatch_pickup_delay,
    defs::DISPATCH_PICKUP_DELAY,
    "Record `apx.dispatch.pickup_delay.duration` if enabled."
);
dispatch_metric!(
    record_materialize,
    materialize_hist,
    dispatch_materialize,
    defs::DISPATCH_MATERIALIZE,
    "Record `apx.dispatch.materialize.duration` if enabled."
);
dispatch_metric!(
    record_queue_depth,
    queue_depth_hist,
    dispatch_queue_depth,
    defs::DISPATCH_QUEUE_DEPTH,
    "Record `apx.dispatch.queue_depth` if enabled."
);
