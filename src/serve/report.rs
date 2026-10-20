//! Errors from the browser: a panic in the dashboard's wasm, most often a
//! hydration mismatch. Each report becomes an error span. When the page sent
//! back the trace it was rendered in, the span joins that trace, so the report
//! sits next to the server render that produced the mismatched HTML.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use serde::Deserialize;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::client_ip;
use crate::observe;

/// A report is a few short strings. Anything bigger is refused unread.
const BODY_LIMIT: usize = 16 * 1024;
const FIELD_LIMIT: usize = 4000;
/// Reports one address may send per window. A page sends at most one.
const PER_WINDOW: u32 = 10;
const WINDOW: Duration = Duration::from_secs(60);
/// Addresses remembered before old windows are dropped.
const ADDRESSES: usize = 10_000;

#[derive(Deserialize)]
struct ClientError {
    message: String,
    /// The last `console.error` before the panic. Leptos writes the details of a
    /// hydration mismatch there: the view that failed and the node it found.
    #[serde(default)]
    detail: String,
    /// Where in the Rust source the panic was raised.
    #[serde(default)]
    location: String,
    /// The page's path, without the query.
    #[serde(default)]
    page: String,
    #[serde(default)]
    traceparent: String,
}

static RECENT: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) async fn client_error(request: Request) -> StatusCode {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let address = client_ip(request.headers(), peer).unwrap_or_default();
    if !allowed(&address, Instant::now()) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE;
    };
    let Ok(report) = serde_json::from_slice::<ClientError>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    record(report);
    StatusCode::NO_CONTENT
}

fn record(report: ClientError) {
    let message = clip(&report.message);
    let detail = clip(&report.detail);
    let location = clip(&report.location);
    let page = clip(report.page.split('?').next().unwrap_or(""));
    let kind = if detail.contains("hydration error") || message.contains("hydration error") {
        "hydration"
    } else {
        "panic"
    };
    let span = tracing::error_span!(
        "client.error",
        otel.name = format!("browser {kind}"),
        otel.kind = "internal",
        "error.type" = kind,
        "url.path" = page.as_str(),
        "code.location" = location.as_str(),
    );
    if let Some(parent) = observe::parent_from(&report.traceparent) {
        let _ = span.set_parent(parent);
    }
    let _enter = span.enter();
    // Named "exception" with the semantic convention fields, so tracing
    // backends show it as an exception. An error event also marks the span failed.
    tracing::error!(
        "exception.type" = kind,
        "exception.message" = message.as_str(),
        "exception.stacktrace" = detail.as_str(),
        "exception"
    );
}

/// Counts a report against its address, a fixed window at a time.
fn allowed(address: &str, now: Instant) -> bool {
    let Ok(mut recent) = RECENT.lock() else {
        return false;
    };
    if recent.len() >= ADDRESSES {
        recent.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
    }
    let entry = recent.entry(address.to_string()).or_insert((now, 0));
    if now.duration_since(entry.0) >= WINDOW {
        *entry = (now, 0);
    }
    entry.1 += 1;
    entry.1 <= PER_WINDOW
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(FIELD_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_gets_a_few_reports_a_minute() {
        let start = Instant::now();
        let address = "203.0.113.7";
        for _ in 0..PER_WINDOW {
            assert!(allowed(address, start));
        }
        assert!(!allowed(address, start));
        assert!(allowed("203.0.113.8", start));
        assert!(allowed(address, start + WINDOW));
    }

    #[test]
    fn a_report_joins_the_trace_of_its_page() {
        use opentelemetry::trace::{Status, TracerProvider};
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        tracing::subscriber::with_default(subscriber, || {
            record(ClientError {
                message: "Unrecoverable hydration error.".into(),
                detail: "A hydration error occurred while trying to hydrate an element defined at app/src/pages/event.rs:12:5.".into(),
                location: "tachys/src/hydration.rs:180:9".into(),
                page: "/servers/99/events/abc?tab=1".into(),
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into(),
            });
        });
        let spans = exporter.get_finished_spans().unwrap();
        let span = spans.first().expect("a span");
        assert_eq!(span.name, "browser hydration");
        assert_eq!(
            span.span_context.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(span.parent_span_id.to_string(), "00f067aa0ba902b7");
        assert!(matches!(span.status, Status::Error { .. }));
        let attribute = |key: &str| {
            span.attributes
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(attribute("url.path").as_deref(), Some("/servers/99/events/abc"));
        let event = span.events.iter().find(|event| event.name == "exception").expect("an exception");
        assert!(event
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "exception.stacktrace" && kv.value.to_string().contains("event.rs:12:5")));
    }

    #[test]
    fn long_fields_are_clipped() {
        let long = "x".repeat(FIELD_LIMIT + 50);
        assert_eq!(clip(&long).chars().count(), FIELD_LIMIT + 1);
        assert_eq!(clip("short"), "short");
    }
}
