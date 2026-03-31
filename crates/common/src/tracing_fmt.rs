//! Shared tracing subscriber setup: `DevAwareFormatter`, `APX_LOG` filter
//! builder, and fmt-only subscriber init.
//!
//! Module is named `tracing_fmt` (not `tracing`) to avoid shadowing the
//! `tracing` crate at downstream use sites.

use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// When `true`, the fmt layer uses the dev-friendly `| apx | out/err |` format
/// instead of the default verbose format with target/file/line.
static DEV_FORMAT: AtomicBool = AtomicBool::new(false);

/// Enable the dev-friendly log format for attached mode.
///
/// Once called, all subsequent tracing events will be formatted as:
/// `YYYY-MM-DD HH:MM:SS.mmm | apx | out | message`
pub fn enable_dev_format() {
    DEV_FORMAT.store(true, Ordering::Relaxed);
}

const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RESET: &str = "\x1b[0m";

/// A tracing event formatter that switches between dev-friendly and verbose
/// formats based on the [`DEV_FORMAT`] flag.
#[derive(Debug, Clone, Copy)]
pub struct DevAwareFormatter;

impl<S, N> FormatEvent<S, N> for DevAwareFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if DEV_FORMAT.load(Ordering::Relaxed) {
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let channel = if *event.metadata().level() == tracing::Level::ERROR {
                "err"
            } else {
                "out"
            };

            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            let message = visitor.0;

            if writer.has_ansi_escapes() {
                writeln!(
                    writer,
                    "{ANSI_YELLOW}{timestamp} |  apx | {channel} | {message}{ANSI_RESET}"
                )
            } else {
                writeln!(writer, "{timestamp} |  apx | {channel} | {message}")
            }
        } else {
            let level = event.metadata().level();
            let role = role_label();

            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);

            writeln!(writer, "{level:>5} | [{role}] {}", visitor.0)
        }
    }
}

/// Return the process role label, cached after the first call.
///
/// Workers have `APX_WORKER_ID` set and produce `"worker-N"`;
/// the supervisor (no env var) produces `"supervisor"`.
fn role_label() -> &'static str {
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL.get_or_init(|| match std::env::var("APX_WORKER_ID") {
        Ok(id) => format!("worker-{id}"),
        Err(_) => "supervisor".to_owned(),
    })
}

/// Visitor that extracts the message field from a tracing event.
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

/// Build an `EnvFilter`-compatible filter string from `APX_LOG`.
///
/// - If `APX_LOG` is a plain level name (e.g. `"debug"`), returns `"{root}={level}"`.
/// - If `APX_LOG` is a full filter spec, returns it as-is.
/// - If `APX_LOG` is unset, defaults to `"{root}=info"`.
#[must_use]
pub fn build_apx_filter(root: &str) -> String {
    match std::env::var("APX_LOG") {
        Ok(level) if is_plain_level(&level) => format!("{root}={level}"),
        Ok(spec) => spec,
        Err(_) => format!("{root}=info"),
    }
}

/// Initialize a fmt-only tracing subscriber using [`DevAwareFormatter`].
///
/// Uses `try_init` so it silently succeeds if a subscriber is already set
/// (e.g. the CLI already called `init_tracing()`).
pub fn init_fmt_subscriber(root: &str) {
    let filter = build_apx_filter(root);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .event_format(DevAwareFormatter)
        .with_filter(EnvFilter::new(filter));

    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}

fn is_plain_level(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    )
}
