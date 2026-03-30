//! Integration tests for native OTLP telemetry: spans, metrics, log forwarding,
//! and trace context propagation.
//!
//! Uses `opentelemetry_sdk` in-memory exporters — no external collector needed.

use crate::with_py;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::metrics::data::Sum;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use pyo3::types::{PyAnyMethods, PyDictMethods};
use std::sync::{Mutex, OnceLock};
use tracing_subscriber::prelude::*;

// ── Shared test providers ───────────────────────────────────────────────
//
// All telemetry tests share one set of in-memory exporters installed as the
// global OTEL providers.  `setup()` is idempotent — safe to call from any test.

struct TestTelemetry {
    span_exporter: InMemorySpanExporter,
    metric_exporter: InMemoryMetricExporter,
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
}

static TEST_TELEMETRY: OnceLock<TestTelemetry> = OnceLock::new();

/// One-time init: install in-memory providers as global OTEL providers and
/// bootstrap the Python telemetry layer (log handler + context var).
///
/// Also installs a tracing subscriber with an OTEL trace layer so that
/// `tracing::info_span!` (used by `python_handler`) exports to the
/// in-memory span exporter.
fn setup() -> &'static TestTelemetry {
    TEST_TELEMETRY.get_or_init(|| {
        let span_exporter = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter.clone())
            .build();

        let metric_exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(metric_exporter.clone()).build();
        let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();

        // Install as global providers so SpanHandle / create_counter use them.
        opentelemetry::global::set_tracer_provider(tracer_provider.clone());
        opentelemetry::global::set_meter_provider(meter_provider.clone());

        // Install a tracing subscriber with OTEL trace layer so that
        // tracing::info_span! in python_handler exports to the in-memory exporter.
        let tracer = tracer_provider.tracer("apx-test");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(tracing_subscriber::EnvFilter::new("info"));
        // try_init: if another test already installed a subscriber, this is a no-op.
        let _ = tracing_subscriber::registry()
            .with(otel_layer)
            .with(fmt_layer)
            .try_init();

        // Bootstrap Python-side telemetry (log handler + context var).
        with_py(|py| {
            crate::telemetry::bootstrap_python_telemetry(py).expect("telemetry bootstrap failed");
        });

        TestTelemetry {
            span_exporter,
            metric_exporter,
            tracer_provider,
            meter_provider,
        }
    })
}

/// Serialize access to exporters (tests run in parallel but share exporters).
static EXPORT_LOCK: Mutex<()> = Mutex::new(());

// ── Span tests ──────────────────────────────────────────────────────────

#[test]
fn span_handle_creates_and_exports_span() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    with_py(|py| {
        // Call SpanHandle through the Python protocol — same path as user code.
        let handle = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create(
                "test.span".to_owned(),
                Some([("key".to_owned(), "val".to_owned())].into()),
            ),
        )
        .unwrap();

        let obj = handle.bind(py);

        // __enter__
        let entered = obj.call_method0("__enter__").unwrap();

        // add_event while the span is open
        entered.call_method1("add_event", ("my_event",)).unwrap();

        // __exit__
        obj.call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();

    assert!(!spans.is_empty(), "expected at least one span, got none");
    let span = spans.iter().find(|s| s.name == "test.span").unwrap();
    assert!(
        span.attributes.iter().any(|kv| kv.key.as_str() == "key"),
        "expected 'key' attribute on span"
    );
    assert!(
        !span.events.events.is_empty(),
        "expected at least one event on span"
    );
    assert_eq!(span.events.events[0].name, "my_event");
}

#[test]
fn span_handle_parent_child_relationship() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    with_py(|py| {
        let parent = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("parent".to_owned(), None),
        )
        .unwrap();

        parent.call_method0(py, "__enter__").unwrap();

        let child = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("child".to_owned(), None),
        )
        .unwrap();

        child.call_method0(py, "__enter__").unwrap();
        child
            .call_method1(py, "__exit__", (py.None(), py.None(), py.None()))
            .unwrap();

        parent
            .call_method1(py, "__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();

    let parent_span = spans.iter().find(|s| s.name == "parent").unwrap();
    let child_span = spans.iter().find(|s| s.name == "child").unwrap();

    // Child's parent_span_id must match parent's span_id.
    assert_eq!(
        child_span.parent_span_id,
        parent_span.span_context.span_id(),
        "child should be parented to the parent span"
    );

    // Both spans should share the same trace_id.
    assert_eq!(
        child_span.span_context.trace_id(),
        parent_span.span_context.trace_id(),
        "child and parent should share the same trace"
    );
}

// ── Metrics tests ───────────────────────────────────────────────────────

#[test]
fn counter_records_to_in_memory_exporter() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.metric_exporter.reset();

    let counter = crate::telemetry::metrics::create_counter(
        "test.requests".to_owned(),
        "total requests".to_owned(),
        String::new(),
    );

    // Call through the Python protocol.
    with_py(|py| {
        let obj = pyo3::Py::new(py, counter).unwrap();
        obj.call_method1(py, "inc", (5u64,)).unwrap();
        obj.call_method1(py, "inc", (3u64,)).unwrap();
    });

    tt.meter_provider.force_flush().unwrap();
    let metrics = tt.metric_exporter.get_finished_metrics().unwrap();

    assert!(!metrics.is_empty(), "expected metric data, got none");
    let found = metrics.iter().any(|rm| {
        rm.scope_metrics
            .iter()
            .any(|sm| sm.metrics.iter().any(|m| m.name == "test.requests"))
    });
    assert!(
        found,
        "expected 'test.requests' counter in exported metrics"
    );
}

#[test]
fn histogram_records_to_in_memory_exporter() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.metric_exporter.reset();

    let histo = crate::telemetry::metrics::create_histogram(
        "test.latency".to_owned(),
        "request latency".to_owned(),
        "ms".to_owned(),
    );

    with_py(|py| {
        let obj = pyo3::Py::new(py, histo).unwrap();
        obj.call_method1(py, "observe", (42.0f64,)).unwrap();
        obj.call_method1(py, "observe", (99.0f64,)).unwrap();
    });

    tt.meter_provider.force_flush().unwrap();
    let metrics = tt.metric_exporter.get_finished_metrics().unwrap();

    let found = metrics.iter().any(|rm| {
        rm.scope_metrics
            .iter()
            .any(|sm| sm.metrics.iter().any(|m| m.name == "test.latency"))
    });
    assert!(
        found,
        "expected 'test.latency' histogram in exported metrics"
    );
}

#[test]
fn gauge_records_to_in_memory_exporter() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.metric_exporter.reset();

    let gauge = crate::telemetry::metrics::create_gauge(
        "test.connections".to_owned(),
        "active connections".to_owned(),
        String::new(),
    );

    with_py(|py| {
        let obj = pyo3::Py::new(py, gauge).unwrap();
        obj.call_method1(py, "set", (42.0f64,)).unwrap();
    });

    tt.meter_provider.force_flush().unwrap();
    let metrics = tt.metric_exporter.get_finished_metrics().unwrap();

    let found = metrics.iter().any(|rm| {
        rm.scope_metrics
            .iter()
            .any(|sm| sm.metrics.iter().any(|m| m.name == "test.connections"))
    });
    assert!(
        found,
        "expected 'test.connections' gauge in exported metrics"
    );
}

#[test]
fn counter_with_attributes() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.metric_exporter.reset();

    let counter = crate::telemetry::metrics::create_counter(
        "test.labeled".to_owned(),
        String::new(),
        String::new(),
    );

    with_py(|py| {
        let obj = pyo3::Py::new(py, counter).unwrap();
        let attrs = pyo3::types::PyDict::new(py);
        attrs.set_item("method", "GET").unwrap();
        obj.call_method1(py, "inc", (1u64, attrs)).unwrap();
    });

    tt.meter_provider.force_flush().unwrap();
    let metrics = tt.metric_exporter.get_finished_metrics().unwrap();

    let found = metrics.iter().any(|rm| {
        rm.scope_metrics
            .iter()
            .any(|sm| sm.metrics.iter().any(|m| m.name == "test.labeled"))
    });
    assert!(found, "expected 'test.labeled' counter in exported metrics");
}

// ── Log forwarding test ─────────────────────────────────────────────────

#[test]
fn emit_log_does_not_panic() {
    // _emit_log writes into the tracing subscriber. We verify it does not
    // panic at any severity level.
    with_py(|py| {
        crate::telemetry::logging::emit_log(
            py,
            50,
            "critical".to_owned(),
            "test".to_owned(),
            String::new(),
        );
        crate::telemetry::logging::emit_log(
            py,
            40,
            "error".to_owned(),
            "test".to_owned(),
            String::new(),
        );
        crate::telemetry::logging::emit_log(
            py,
            30,
            "warning".to_owned(),
            "test".to_owned(),
            String::new(),
        );
        crate::telemetry::logging::emit_log(
            py,
            20,
            "info".to_owned(),
            "test".to_owned(),
            String::new(),
        );
        crate::telemetry::logging::emit_log(
            py,
            10,
            "debug".to_owned(),
            "test".to_owned(),
            String::new(),
        );
        crate::telemetry::logging::emit_log(
            py,
            5,
            "trace".to_owned(),
            "test".to_owned(),
            String::new(),
        );
    });
}

// ── Context propagation test ────────────────────────────────────────────

#[test]
fn trace_context_roundtrip_through_python_context_var() {
    setup();

    with_py(|py| {
        // Write a known context into the Python ContextVar.
        let ctx = crate::telemetry::context::TraceContext {
            trace_id: [0xAB; 16],
            span_id: [0xCD; 8],
            trace_flags: 1,
            trace_state: String::new(),
        };
        crate::telemetry::context::set_python_context(py, &ctx).unwrap();

        // Read it back via the ContextVar (same mechanism SpanHandle uses).
        let cv = crate::telemetry::context::context_var().unwrap();
        let val: (String, String, u8, String) =
            cv.call_method0(py, c"get").unwrap().extract(py).unwrap();

        assert_eq!(val.0, "abababababababababababababababab"); // 16 × 0xAB
        assert_eq!(val.1, "cdcdcdcdcdcdcdcd"); // 8 × 0xCD
        assert_eq!(val.2, 1);
        assert_eq!(val.3, "");
    });
}

#[test]
fn span_handle_inherits_injected_trace_context() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    let injected_trace_id = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10,
    ];
    let injected_span_id = [0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8];

    with_py(|py| {
        // Simulate Rust request span injecting context (like dispatch does).
        let ctx = crate::telemetry::context::TraceContext {
            trace_id: injected_trace_id,
            span_id: injected_span_id,
            trace_flags: 1,
            trace_state: String::new(),
        };
        crate::telemetry::context::set_python_context(py, &ctx).unwrap();

        // Create a SpanHandle — it should become a child of the injected context.
        let handle = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("child_of_injected".to_owned(), None),
        )
        .unwrap();

        handle.call_method0(py, "__enter__").unwrap();
        handle
            .call_method1(py, "__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();

    let child = spans
        .iter()
        .find(|s| s.name == "child_of_injected")
        .unwrap();

    // The child span should carry the injected trace_id.
    assert_eq!(
        child.span_context.trace_id().to_bytes(),
        injected_trace_id,
        "child span should inherit the injected trace_id"
    );

    // The child's parent_span_id should be the injected span_id.
    assert_eq!(
        child.parent_span_id.to_bytes(),
        injected_span_id,
        "child span should be parented to the injected span_id"
    );
}

// ── Additional span tests ──────────────────────────────────────────────

#[test]
fn span_handle_set_attribute() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    with_py(|py| {
        let handle = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("attr.span".to_owned(), None),
        )
        .unwrap();

        handle.call_method0(py, "__enter__").unwrap();
        handle
            .call_method1(py, "set_attribute", ("region", "us-east-1"))
            .unwrap();
        handle
            .call_method1(py, "__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();
    let span = spans.iter().find(|s| s.name == "attr.span").unwrap();

    assert!(
        span.attributes.iter().any(|kv| kv.key.as_str() == "region"),
        "expected 'region' attribute set via set_attribute"
    );
}

#[test]
fn span_handle_async_context_manager() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    with_py(|py| {
        // __aenter__ returns a coroutine — verify it doesn't panic and
        // the span is exported after __aexit__.
        let handle = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("async.span".to_owned(), None),
        )
        .unwrap();

        // __aenter__ returns a coroutine wrapping the entered handle.
        let aenter_coro = handle.call_method0(py, "__aenter__").unwrap();
        // Drive the coroutine to completion with asyncio.run.
        let asyncio = py.import(c"asyncio").unwrap();
        asyncio.call_method1(c"run", (aenter_coro,)).unwrap();

        // __aexit__ returns a coroutine wrapping False.
        let aexit_coro = handle
            .call_method1(py, "__aexit__", (py.None(), py.None(), py.None()))
            .unwrap();
        asyncio.call_method1(c"run", (aexit_coro,)).unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();

    assert!(
        spans.iter().any(|s| s.name == "async.span"),
        "expected 'async.span' from async context manager"
    );
}

// ── Exception capture tests ─────────────────────────────────────────────

#[test]
fn span_handle_exit_with_exception_records_error() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.span_exporter.reset();

    with_py(|py| {
        let handle = pyo3::Py::new(
            py,
            crate::telemetry::spans::SpanHandle::create("erroring.span".to_owned(), None),
        )
        .unwrap();

        let obj = handle.bind(py);
        obj.call_method0("__enter__").unwrap();

        // Raise a ValueError inside Python and capture exc_info, then pass
        // the triple to __exit__ exactly like Python's `with` statement.
        let code = c"
import sys
try:
    raise ValueError('something broke')
except:
    exc_info = sys.exc_info()
";
        let locals = pyo3::types::PyDict::new(py);
        py.run(code, None, Some(&locals)).unwrap();

        let exc_info = locals.get_item("exc_info").unwrap().unwrap();
        let exc_type = exc_info.get_item(0).unwrap();
        let exc_val = exc_info.get_item(1).unwrap();
        let exc_tb = exc_info.get_item(2).unwrap();

        obj.call_method1("__exit__", (&exc_type, &exc_val, &exc_tb))
            .unwrap();
    });

    tt.tracer_provider.force_flush().unwrap();
    let spans = tt.span_exporter.get_finished_spans().unwrap();

    let span = spans
        .iter()
        .find(|s| s.name == "erroring.span")
        .expect("expected 'erroring.span' in exported spans");

    assert_eq!(
        span.status,
        opentelemetry::trace::Status::error("something broke"),
        "span status should be Error with the exception message"
    );

    let exc_event = span
        .events
        .events
        .iter()
        .find(|e| e.name == "exception")
        .expect("expected an 'exception' event on the span");

    let attr = |key: &str| -> Option<String> {
        exc_event.attributes.iter().find_map(|kv| {
            if kv.key.as_str() == key {
                Some(kv.value.to_string())
            } else {
                None
            }
        })
    };

    assert_eq!(
        attr("exception.type").as_deref(),
        Some("ValueError"),
        "exception.type should be ValueError"
    );
    assert!(
        attr("exception.message")
            .as_ref()
            .is_some_and(|m| m.contains("something broke")),
        "exception.message should contain 'something broke'"
    );
    assert!(
        attr("exception.stacktrace").is_some(),
        "exception.stacktrace should be present"
    );
}

// ── Additional metrics tests ───────────────────────────────────────────

#[test]
fn counter_value_is_correct() {
    let tt = setup();
    let _lock = EXPORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tt.metric_exporter.reset();

    let counter = crate::telemetry::metrics::create_counter(
        "test.sum_check".to_owned(),
        String::new(),
        String::new(),
    );

    with_py(|py| {
        let obj = pyo3::Py::new(py, counter).unwrap();
        obj.call_method1(py, "inc", (5u64,)).unwrap();
        obj.call_method1(py, "inc", (3u64,)).unwrap();
    });

    tt.meter_provider.force_flush().unwrap();
    let metrics = tt.metric_exporter.get_finished_metrics().unwrap();

    // Drill into the metric data to verify 5 + 3 = 8.
    let mut found_value = None;
    for rm in &metrics {
        for sm in &rm.scope_metrics {
            for m in &sm.metrics {
                if m.name == "test.sum_check"
                    && let Some(sum) = m.data.as_any().downcast_ref::<Sum<u64>>()
                {
                    found_value = sum.data_points.first().map(|dp| dp.value);
                }
            }
        }
    }

    assert_eq!(found_value, Some(8), "expected counter value 5 + 3 = 8");
}

// ── Additional log tests ───────────────────────────────────────────────

#[test]
fn emit_log_through_python_handler() {
    setup();

    with_py(|py| {
        // Use Python logging module — our handler forwards to Rust tracing.
        let code = c"
import logging
logging.getLogger('test.handler').warning('hello from python')
";
        // Should not panic — the handler calls _emit_log internally.
        py.run(code, None, None).unwrap();
    });
}

// ── Full HTTP request tests removed (depend on TestServer) ─────────────
// Require TestServer infrastructure not yet available in this crate.
