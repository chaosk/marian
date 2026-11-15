//! Logs go to stdout: plain text in a terminal, and JSON lines anywhere else
//! (`MARIAN_LOG_FORMAT=text|json` overrides), so a collector like Loki reads
//! each line's level and fields instead of guessing at coloured text. Traces leave the process only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set, and metrics only when
//! `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` is: a trace store like Tempo takes no
//! metrics, so they don't follow the traces on their own.

use std::env;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use diesel::connection::{Instrumentation, InstrumentationEvent};
use opentelemetry::trace::{Span, SpanKind, Tracer, TracerProvider};
use opentelemetry::{Context, KeyValue, global};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Instrument;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const STATEMENT_LIMIT: usize = 500;

static EXPORT: AtomicBool = AtomicBool::new(false);

struct PendingQuery {
    start: SystemTime,
    statement: String,
    operation: String,
    system: &'static str,
}

pub struct Telemetry {
    provider: Option<SdkTracerProvider>,
    meters: Option<SdkMeterProvider>,
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            if let Err(err) = provider.shutdown() {
                eprintln!("OpenTelemetry shutdown failed: {err}");
            }
        }
        if let Some(meters) = self.meters.take() {
            if let Err(err) = meters.shutdown() {
                eprintln!("OpenTelemetry metrics shutdown failed: {err}");
            }
        }
    }
}

pub fn init() -> Telemetry {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "marian=info,tower_http=info".into());
    let logs = match log_format(
        env::var("MARIAN_LOG_FORMAT").ok().as_deref(),
        std::io::stdout().is_terminal(),
    ) {
        LogFormat::Json => json_logs(std::io::stdout).boxed(),
        LogFormat::Text { colour } => tracing_subscriber::fmt::layer().with_ansi(colour).boxed(),
    }
    .with_filter(filter);

    let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let provider = endpoint.as_deref().and_then(exporter);
    let meters = env::var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .as_deref()
        .and_then(metric_exporter);
    if let Some(meters) = &meters {
        global::set_meter_provider(meters.clone());
    }

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
    Telemetry { provider, meters }
}

#[derive(Debug, PartialEq, Eq)]
enum LogFormat {
    Json,
    Text { colour: bool },
}

/// One JSON object per line: `level`, `target` and the message at the top,
/// and the fields of the span the event happened in under `span`.
fn json_logs<S, W>(writer: W) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(writer)
}

/// A terminal gets coloured text; a pipe or a container's log gets JSON unless
/// asked for text, which then comes without colour codes.
fn log_format(setting: Option<&str>, terminal: bool) -> LogFormat {
    match setting
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => LogFormat::Json,
        Some("text") => LogFormat::Text { colour: terminal },
        _ if terminal => LogFormat::Text { colour: true },
        _ => LogFormat::Json,
    }
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
    Some(
        SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource())
            .build(),
    )
}

/// Metrics are posted once a minute to the endpoint as given, as OTLP does
/// with a signal's own endpoint: a full URL, like `http://mimir/otlp/v1/metrics`.
fn metric_exporter(endpoint: &str) -> Option<SdkMeterProvider> {
    use opentelemetry_otlp::WithExportConfig;

    let built = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build();
    let exporter = match built {
        Ok(exporter) => exporter,
        Err(err) => {
            eprintln!("OpenTelemetry metrics export is off: {err}");
            return None;
        }
    };
    Some(
        SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter).build())
            .with_resource(resource())
            .build(),
    )
}

fn resource() -> Resource {
    let mut resource = Resource::builder();
    if env::var("OTEL_SERVICE_NAME")
        .ok()
        .is_none_or(|value| value.trim().is_empty())
    {
        resource = resource.with_service_name("marian");
    }
    // Compiled in by build.rs: a release number from a tag, or the commit from a branch.
    resource
        .with_attribute(KeyValue::new("service.version", env!("MARIAN_VERSION")))
        .build()
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

/// What becomes a span in the exported traces. Serenity's client, shard
/// manager and shard runner each hold a span for as long as the gateway stays
/// connected, and every heartbeat and received event lands inside it, so one
/// trace would grow for days. Only its REST calls are kept; each Discord event
/// gets its own root span in the gateway instead.
fn trace_filter() -> EnvFilter {
    EnvFilter::new("info")
        .add_directive("hyper=off".parse().expect("hyper filter"))
        .add_directive("h2=off".parse().expect("h2 filter"))
        .add_directive("reqwest=off".parse().expect("reqwest filter"))
        .add_directive("tonic=off".parse().expect("tonic filter"))
        .add_directive("serenity=off".parse().expect("serenity filter"))
        .add_directive("serenity::http=info".parse().expect("serenity http filter"))
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

pub fn install_query_trace(conn: &mut crate::store::Connection, system: &'static str) {
    use diesel_async::AsyncConnection;
    conn.set_instrumentation(QueryTrace {
        system,
        pending: None,
    });
}

/// Each connection keeps the query it's running: a task's query can start on
/// one worker thread and finish on another. Marian sends one query at a time
/// on a connection, so a query that starts while another is pending replaces
/// it; that one's future was dropped before it finished.
struct QueryTrace {
    system: &'static str,
    pending: Option<PendingQuery>,
}

impl Instrumentation for QueryTrace {
    fn on_connection_event(&mut self, event: InstrumentationEvent<'_>) {
        match event {
            InstrumentationEvent::StartQuery { query, .. } => {
                self.pending = Some(start_query(self.system, &query.to_string()));
            }
            InstrumentationEvent::FinishQuery { .. } => {
                if let Some(pending) = self.pending.take() {
                    finish_query(pending);
                }
            }
            _ => {}
        }
    }
}

fn start_query(system: &'static str, sql: &str) -> PendingQuery {
    PendingQuery {
        start: SystemTime::now(),
        statement: statement_shape(sql),
        operation: operation(sql),
        system,
    }
}

fn finish_query(pending: PendingQuery) {
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
    let Some(parent) = query_parent() else {
        return;
    };
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
        .start_with_context(&tracer, &parent);
    let end = pending.start.checked_add(duration).unwrap_or(pending.start);
    span.end_with_timestamp(end);
}

/// The span a query belongs to. A query outside any span, such as the
/// readiness probe's or the pool's check before handing out a connection,
/// would be a trace of its own, so it is only logged.
fn query_parent() -> Option<Context> {
    use opentelemetry::trace::TraceContextExt;
    let current = Context::current();
    current.has_active_span().then_some(current)
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
    use super::{
        EXPORT, LogFormat, json_logs, log_format, parent_from, statement_shape, trace_filter,
        traceparent, traces_endpoint,
    };

    #[test]
    fn logs_are_json_unless_a_terminal_reads_them() {
        assert_eq!(log_format(None, false), LogFormat::Json);
        assert_eq!(log_format(None, true), LogFormat::Text { colour: true });
        assert_eq!(log_format(Some(" JSON "), true), LogFormat::Json);
        assert_eq!(
            log_format(Some("text"), false),
            LogFormat::Text { colour: false }
        );
        assert_eq!(log_format(Some("loud"), false), LogFormat::Json);
    }

    #[test]
    fn a_json_line_carries_its_level_and_span_fields() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::registry().with(json_logs(move || writer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("http", otel.name = "GET /api/servers");
            let _entered = span.enter();
            tracing::warn!("discord guilds returned 429 Too Many Requests");
        });

        let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains('\u{1b}'), "no colour codes: {text}");
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["level"], "WARN");
        assert_eq!(
            line["message"],
            "discord guilds returned 429 Too Many Requests"
        );
        assert_eq!(line["span"]["otel.name"], "GET /api/servers");
    }

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
            assert_eq!(
                written,
                format!("00-{}-{}-01", ids.trace_id(), ids.span_id())
            );
            let parent = parent_from(&written).unwrap();
            assert_eq!(parent.span().span_context().span_id(), ids.span_id());
        });
    }

    #[test]
    fn a_traceparent_names_its_parent() {
        use opentelemetry::trace::TraceContextExt;

        let parent =
            parent_from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").unwrap();
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

    #[test]
    fn a_query_outside_any_span_is_not_a_trace() {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
        use tracing_subscriber::layer::SubscriberExt;

        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(InMemorySpanExporter::default())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::with_default(subscriber, || {
            assert!(super::query_parent().is_none());
            tracing::info_span!("tick").in_scope(|| {
                assert!(
                    super::query_parent().is_some(),
                    "a query in a span joins it"
                );
            });
            assert!(super::query_parent().is_none());
        });
    }

    #[test]
    fn the_gateway_connection_is_not_a_trace() {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("test"))
                .with_filter(trace_filter()),
        );
        tracing::subscriber::with_default(subscriber, || {
            // Serenity's shard runner, entered for as long as the gateway is up.
            let runner =
                tracing::info_span!(target: "serenity::gateway::bridge::shard_runner", "run");
            let _runner = runner.enter();
            tracing::info_span!(target: "serenity::gateway::bridge::shard_runner", "recv_event")
                .in_scope(|| {});
            let event = tracing::info_span!(parent: None, "discord_event");
            event.in_scope(|| {
                tracing::info_span!(target: "serenity::http::ratelimiting", "perform")
                    .in_scope(|| {});
            });
        });
        let spans = exporter.get_finished_spans().expect("spans");
        let names: Vec<&str> = spans.iter().map(|span| span.name.as_ref()).collect();
        assert_eq!(names, ["perform", "discord_event"]);
        let event = &spans[1];
        assert_eq!(event.parent_span_id, opentelemetry::trace::SpanId::INVALID);
        assert_eq!(spans[0].parent_span_id, event.span_context.span_id());
    }
}
