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
use super::http::framework_meter;

// ── Global toggles ────────────────────────────────────────────────────────

static TOGGLES: OnceLock<ApxMetricToggles> = OnceLock::new();

/// Initialize APX dispatch metric toggles for this worker process.
///
/// Must be called once after reading the Python telemetry config.
/// Subsequent calls are silently ignored (OnceLock semantics).
pub fn init(toggles: ApxMetricToggles) {
    let _ = TOGGLES.set(toggles);
}

/// Return the active APX metric toggles.
///
/// Falls back to all-disabled defaults if [`init`] has not been called.
fn toggles() -> &'static ApxMetricToggles {
    static DEFAULT: ApxMetricToggles = ApxMetricToggles {
        dispatch_body_collect: false,
        dispatch_crossbeam_send: false,
        dispatch_response_wait: false,
        dispatch_total: false,
        asgi_receive_build: false,
        asgi_send_parse: false,
    };
    TOGGLES.get().unwrap_or(&DEFAULT)
}

// ── Metric declarations ───────────────────────────────────────────────────

const NO_ATTRS: &[opentelemetry::KeyValue] = &[];

/// Generate a lazy histogram getter and a gated public `record_*` function.
///
/// Arguments: `(record_fn, hist_fn, toggle_field, metric_name, description)`
///
/// Each `static INST` is scoped inside its own `$hist_fn` body, so all six
/// metrics live flat in this module with no helper sub-modules needed.
macro_rules! dispatch_metric {
    ($record_fn:ident, $hist_fn:ident, $toggle:ident, $metric_name:expr, $description:expr) => {
        fn $hist_fn() -> &'static Histogram<f64> {
            static INST: OnceLock<Histogram<f64>> = OnceLock::new();
            INST.get_or_init(|| {
                framework_meter()
                    .f64_histogram($metric_name)
                    .with_description($description)
                    .with_unit("us")
                    .build()
            })
        }

        #[doc = concat!("Record `", $metric_name, "` if enabled.")]
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
    "apx.dispatch.body_collect.duration",
    "Time to collect the request body from the network stream"
);
dispatch_metric!(
    record_crossbeam_send,
    crossbeam_send_hist,
    dispatch_crossbeam_send,
    "apx.dispatch.crossbeam_send.duration",
    "Time to push the request slot to the crossbeam channel and signal wakeup"
);
dispatch_metric!(
    record_response_wait,
    response_wait_hist,
    dispatch_response_wait,
    "apx.dispatch.response_wait.duration",
    "Time waiting for the Python handler to produce a response"
);
dispatch_metric!(
    record_dispatch_total,
    dispatch_total_hist,
    dispatch_total,
    "apx.dispatch.total.duration",
    "Total dispatch duration from body collect start to response ready"
);
dispatch_metric!(
    record_receive_build,
    receive_build_hist,
    asgi_receive_build,
    "apx.asgi.receive_build.duration",
    "Time to build the ASGI receive dict for the Python handler"
);
dispatch_metric!(
    record_send_parse,
    send_parse_hist,
    asgi_send_parse,
    "apx.asgi.send_parse.duration",
    "Time to parse the ASGI send event dict from the Python handler"
);
