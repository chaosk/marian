//! Logs always go to stderr. Traces leave the process only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

use std::cell::RefCell;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use diesel::connection::{Connection, Instrumentation, InstrumentationEvent};
use opentelemetry::trace::{Span, SpanKind, Tracer, TracerProvider};
use opentelemetry::{global, Context, KeyValue};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing::Instrument;
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
    system: &'static str,
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

    // with_endpoint posts to the URL as given. The SDK only appends /v1/traces
    // when it reads OTEL_EXPORTER_OTLP_ENDPOINT itself.
    let endpoint = traces_endpoint(endpoint);
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
    let mut resource = Resource::builder();
    if env::var("OTEL_SERVICE_NAME")
        .ok()
        .is_none_or(|value| value.trim().is_empty())
    {
        resource = resource.with_service_name("marian");
    }
    // Compiled in by build.rs: a release number from a tag, or the commit from a branch.
    let resource = resource
        .with_attribute(KeyValue::new("service.version", env!("MARIAN_VERSION")))
        .build();
    Some(
        SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build(),
    )
}

/// OTLP/HTTP traces are posted to `{base}/v1/traces`. A value that already
/// ends in that path is left alone.
fn traces_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
    }
}

fn trace_filter() -> EnvFilter {
    EnvFilter::new("info")
        .add_directive("hyper=off".parse().expect("hyper filter"))
        .add_directive("h2=off".parse().expect("h2 filter"))
        .add_directive("reqwest=off".parse().expect("reqwest filter"))
        .add_directive("tonic=off".parse().expect("tonic filter"))
}

/// The trace a page was rendered in, as a W3C `traceparent`. The page carries
/// it so a report from the browser joins the trace of the render it came from.
#[derive(Clone, Debug)]
pub struct PageTrace(pub String);

/// The current span as a `traceparent`, when traces are exported.
pub fn traceparent() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    if !EXPORT.load(Ordering::Relaxed) {
        return None;
    }
    let context = tracing::Span::current().context();
    let span = context.span();
    let found = span.span_context();
    found.is_valid().then(|| {
        format!(
            "00-{}-{}-{:02x}",
            found.trace_id(),
            found.span_id(),
            found.trace_flags().to_u8()
        )
    })
}

/// The parent a `traceparent` names, or nothing when it doesn't parse.
pub fn parent_from(traceparent: &str) -> Option<Context> {
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::TraceContextExt;

    let carrier = std::collections::HashMap::from([(
        "traceparent".to_string(),
        traceparent.trim().to_string(),
    )]);
    let context = opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&carrier);
    context.span().span_context().is_valid().then_some(context)
}

pub fn install_query_trace(conn: &mut impl Connection, system: &'static str) {
    conn.set_instrumentation(QueryTrace { system });
}

struct QueryTrace {
    system: &'static str,
}

impl Instrumentation for QueryTrace {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartQuery { query, .. } => {
                start_query(self.system, &query.to_string())
            }
            InstrumentationEvent::FinishQuery { .. } => finish_query(),
            _ => {}
        }
    }
}

fn start_query(system: &'static str, sql: &str) {
    let pending = PendingQuery {
        start: SystemTime::now(),
        statement: statement_shape(sql),
        operation: operation(sql),
        system,
    };
    QUERIES.with(|stack| {
        if let Ok(mut stack) = stack.try_borrow_mut() {
            stack.push(pending);
        }
    });
}

fn finish_query() {
    let pending = QUERIES.with(|stack| {
        stack
            .try_borrow_mut()
            .ok()
            .and_then(|mut stack| stack.pop())
    });
    let Some(pending) = pending else {
        return;
    };
    let duration = pending.start.elapsed().unwrap_or_default();
    let millis = duration.as_secs_f64() * 1000.0;
    tracing::debug!(
        "db.system" = pending.system,
        "db.operation" = pending.operation.as_str(),
        "db.statement" = pending.statement.as_str(),
        "db.duration_ms" = millis,
        "query"
    );
    if !EXPORT.load(Ordering::Relaxed) {
        return;
    }
    let tracer = global::tracer("marian");
    let name = if pending.statement.is_empty() {
        pending.system.to_string()
    } else {
        pending.statement.clone()
    };
    let mut span = tracer
        .span_builder(name)
        .with_kind(SpanKind::Client)
        .with_start_time(pending.start)
        .with_attributes([
            KeyValue::new("db.system", pending.system),
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

/// One outbound HTTP call, named by method and path. The query string stays off
/// the span: Google and Discord put tokens there.
pub(crate) async fn send_traced(
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, reqwest::Error> {
    let (client, built) = builder.build_split();
    let request = built?;
    let method = request.method().to_string();
    let host = request.url().host_str().unwrap_or("").to_string();
    let path = request.url().path().to_string();
    let span = tracing::info_span!(
        "http",
        otel.kind = "client",
        "http.request.method" = method.as_str(),
        "server.address" = host.as_str(),
        "url.path" = path.as_str(),
        "http.response.status_code" = tracing::field::Empty,
    );
    async move {
        let response = client.execute(request).await?;
        tracing::Span::current().record("http.response.status_code", response.status().as_u16());
        Ok(response)
    }
    .instrument(span)
    .await
}

fn push_limited(out: &mut String, c: char) {
    if out.len() + c.len_utf8() <= STATEMENT_LIMIT {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::{parent_from, statement_shape, traceparent, traces_endpoint, EXPORT};

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

    #[test]
    fn a_page_carries_the_span_it_was_rendered_in() {
        use opentelemetry::trace::{TraceContextExt, TracerProvider};
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use std::sync::atomic::Ordering;
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        use tracing_subscriber::layer::SubscriberExt;

        EXPORT.store(true, Ordering::Relaxed);
        let provider = SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("http");
            let _enter = span.enter();
            let written = traceparent().expect("a traceparent");
            let context = span.context();
            let current = context.span();
            let ids = current.span_context();
            assert_eq!(written, format!("00-{}-{}-01", ids.trace_id(), ids.span_id()));
            let parent = parent_from(&written).unwrap();
            assert_eq!(parent.span().span_context().span_id(), ids.span_id());
        });
    }

    #[test]
    fn a_traceparent_names_its_parent() {
        use opentelemetry::trace::TraceContextExt;

        let parent = parent_from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").unwrap();
        let span = parent.span();
        assert_eq!(
            span.span_context().trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert!(parent_from("").is_none());
        assert!(parent_from("not a trace").is_none());
    }

    #[test]
    fn traces_endpoint_appends_the_signal_path() {
        assert_eq!(
            traces_endpoint("http://tempo:4318"),
            "http://tempo:4318/v1/traces"
        );
        assert_eq!(
            traces_endpoint("http://tempo:4318/"),
            "http://tempo:4318/v1/traces"
        );
        assert_eq!(
            traces_endpoint("http://collector.example/otlp"),
            "http://collector.example/otlp/v1/traces"
        );
        assert_eq!(
            traces_endpoint("http://tempo:4318/v1/traces"),
            "http://tempo:4318/v1/traces"
        );
        assert_eq!(
            traces_endpoint("http://tempo:4318/v1/traces/"),
            "http://tempo:4318/v1/traces"
        );
    }
}
