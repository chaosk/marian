//! The browser half of Marian's dashboard. A panic here, most often a hydration
//! mismatch, is written to the console and sent to `/api/client-errors`, which
//! records it in the trace the page was rendered in.

use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, Ordering};

use wasm_bindgen::{JsCast, JsValue};

/// Where the console wrapper leaves the last error it saw.
const LAST_ERROR: &str = "__marianLastError";

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    keep_last_console_error();
    std::panic::set_hook(Box::new(|info| {
        // Read before the console hook writes the panic itself.
        let detail = last_console_error();
        console_error_panic_hook::hook(info);
        report(info, detail);
    }));
    leptos::mount::hydrate_body(marian_app::App);
}

/// Leptos explains a hydration mismatch in a `console.error` just before it
/// panics: the view that failed and the node it found instead. The panic
/// message only says to read that, so the wrapper keeps a copy for the report.
fn keep_last_console_error() {
    let install = js_sys::Function::new_no_args(&format!(
        r#"
        const original = console.error;
        console.error = function (...args) {{
          try {{
            window.{LAST_ERROR} = args
              .map((arg) => arg instanceof Node
                ? (arg.outerHTML ?? arg.textContent ?? arg.nodeName).slice(0, 600)
                : String(arg))
              .join(" ")
              .slice(0, 4000);
          }} catch (_) {{}}
          return original.apply(this, args);
        }};
        "#
    ));
    let _ = install.call0(&JsValue::NULL);
}

fn last_console_error() -> String {
    web_sys::window()
        .and_then(|window| js_sys::Reflect::get(&window, &JsValue::from_str(LAST_ERROR)).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

/// One beacon per page: the first panic is the cause, later ones follow from it.
fn report(info: &PanicHookInfo, detail: String) {
    static SENT: AtomicBool = AtomicBool::new(false);
    if SENT.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let message = info
        .payload_as_str()
        .map(str::to_string)
        .unwrap_or_else(|| info.to_string());
    let location = info
        .location()
        .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
        .unwrap_or_default();
    let traceparent = window
        .document()
        .and_then(|document| document.query_selector("meta[name=traceparent]").ok().flatten())
        .and_then(|meta| meta.get_attribute("content"))
        .unwrap_or_default();
    let body = serde_json::json!({
        "message": message,
        "detail": detail,
        "location": location,
        "page": window.location().pathname().unwrap_or_default(),
        "traceparent": traceparent,
    })
    .to_string();
    // A beacon is queued by the browser, so it still goes out when the wasm aborts.
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/json");
    let parts = js_sys::Array::of1(&JsValue::from_str(&body));
    if let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(parts.unchecked_ref(), &options) {
        let _ = window
            .navigator()
            .send_beacon_with_opt_blob(&marian_app::api::path::client_errors(), Some(&blob));
    }
}
