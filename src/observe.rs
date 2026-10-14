//! Logs always go to stderr. Traces leave the process only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

use std::cell::RefCell;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use opentelemetry::trace::{Span, Tracer, TracerProvider};
use opentelemetry::{global, Context, KeyValue};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use rusqlite::Connection;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

const STATEMENT_LIMIT: usize = 500;

static EXPORT: AtomicBool = AtomicBool::new(false);

struct PendingQuery {
    start: SystemTime,
    statement: String,
    operation: String,
}

thread_local! {
    static QUERIES: RefCell<Vec<PendingQuery>> = const { RefCell::new(Vec::new()) };
}

pub struct Telemetry {
    provider: Option<SdkTracerProvider>,
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            if let Err(err) = provider.shutdown() {
                eprintln!("OpenTelemetry shutdown failed: {err}");
            }
        }
    }
}

pub fn init() -> Telemetry {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "marian=info,tower_http=info".into());
    let logs = tracing_subscriber::fmt::layer().with_filter(filter);

    let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let provider = endpoint.as_deref().and_then(exporter);

    let traces = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("marian"))
            .with_filter(trace_filter())
    });
    if let Some(provider) = &provider {
        global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        global::set_tracer_provider(provider.clone());
        EXPORT.store(true, Ordering::Relaxed);
    }

    tracing_subscriber::registry()
        .with(logs)
        .with(traces)
        .init();
    Telemetry { provider }
}

fn exporter(endpoint: &str) -> Option<SdkTracerProvider> {
    use opentelemetry_otlp::WithExportConfig;

    let built = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build();
    let exporter = match built {
        Ok(exporter) => exporter,
        Err(err) => {
            eprintln!("OpenTelemetry export is off: {err}");
            return None;
        }
    };
    let resource = if env::var("OTEL_SERVICE_NAME")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        Resource::builder().build()
    } else {
        Resource::builder().with_service_name("marian").build()
    };
    Some(
        SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build(),
    )
}

fn trace_filter() -> EnvFilter {
    EnvFilter::new("info")
        .add_directive("hyper=off".parse().expect("hyper filter"))
        .add_directive("h2=off".parse().expect("h2 filter"))
        .add_directive("reqwest=off".parse().expect("reqwest filter"))
        .add_directive("tonic=off".parse().expect("tonic filter"))
}

pub fn install_query_trace(conn: &mut Connection) {
    conn.trace(Some(start_query));
    conn.profile(Some(finish_query));
}

fn start_query(sql: &str) {
    let pending = PendingQuery {
        start: SystemTime::now(),
        statement: statement_shape(sql),
        operation: operation(sql),
    };
    QUERIES.with(|stack| {
        if let Ok(mut stack) = stack.try_borrow_mut() {
            stack.push(pending);
        }
    });
}

fn finish_query(_sql: &str, duration: Duration) {
    let pending = QUERIES.with(|stack| {
        stack
            .try_borrow_mut()
            .ok()
            .and_then(|mut stack| stack.pop())
    });
    let Some(pending) = pending else {
        return;
    };
    if duration < Duration::from_millis(1) {
        return;
    }
    let millis = duration.as_secs_f64() * 1000.0;
    tracing::debug!(
        "db.system" = "sqlite",
        "db.operation" = pending.operation.as_str(),
        "db.statement" = pending.statement.as_str(),
        "db.duration_ms" = millis,
        "sqlite query"
    );
    if !EXPORT.load(Ordering::Relaxed) {
        return;
    }
    let tracer = global::tracer("marian");
    let mut span = tracer
        .span_builder("sqlite")
        .with_start_time(pending.start)
        .with_attributes([
            KeyValue::new("db.system", "sqlite"),
            KeyValue::new("db.operation", pending.operation),
            KeyValue::new("db.statement", pending.statement),
            KeyValue::new("db.duration_ms", millis),
        ])
        .start_with_context(&tracer, &Context::current());
    let end = pending.start.checked_add(duration).unwrap_or(pending.start);
    span.end_with_timestamp(end);
}

fn operation(sql: &str) -> String {
    sql.split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(16)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Expanded SQL keeps bound values. Replace quoted text and numbers before
/// anything is recorded.
fn statement_shape(sql: &str) -> String {
    let mut out = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if out.len() >= STATEMENT_LIMIT {
            break;
        }
        if c == '\'' {
            push_limited(&mut out, '?');
            while let Some(next) = chars.next() {
                if next == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        continue;
                    }
                    break;
                }
            }
        } else if c.is_ascii_digit() {
            push_limited(&mut out, '?');
            while chars.peek().is_some_and(|next| next.is_ascii_digit()) {
                chars.next();
            }
        } else {
            push_limited(&mut out, c);
        }
    }
    out
}

fn push_limited(out: &mut String, c: char) {
    if out.len() + c.len_utf8() <= STATEMENT_LIMIT {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::statement_shape;

    #[test]
    fn statement_shape_hides_values() {
        assert_eq!(
            statement_shape("SELECT 'Game night' FROM events"),
            "SELECT ? FROM events"
        );
        assert_eq!(
            statement_shape("SELECT 'it''s fine' WHERE id = 42"),
            "SELECT ? WHERE id = ?"
        );
        assert_eq!(
            statement_shape("UPDATE events SET title = 'a'"),
            "UPDATE events SET title = ?"
        );
        let long = format!("SELECT '{}'", "n".repeat(600));
        assert!(statement_shape(&long).len() <= 500);
        assert!(!statement_shape(&long).contains('n'));
    }
}
