//! Marian's web pages, rendered on the server and hydrated in the browser.

#![recursion_limit = "256"]

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, HashedStylesheet, Link, Meta, MetaTags, Title};
use leptos_router::components::{ParentRoute, Route, Router, Routes};
use leptos_router::hooks::{use_params_map, use_query_map};
use leptos_router::path;

pub mod api;
mod client;
mod format;
mod widgets;
mod markdown;
mod pages;
mod cache;
mod shell;

use api::Failure;

pub(crate) use leptos_router::components::A;

pub fn shell(options: LeptosOptions) -> impl IntoView {
    #[cfg(feature = "ssr")]
    send_page_policy();
    let site_json = api::site_json();
    let notice_json = api::notice_json();
    let trace = api::page_trace();
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                {trace.map(|trace| view! { <meta name="traceparent" content=trace/> })}
                <script id="marian-site" type="application/json" inner_html=site_json></script>
                <script id="marian-notice" type="application/json" inner_html=notice_json></script>
                <AutoReload options=options.clone()/>
                <HashedStylesheet options=options.clone() id="marian"/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// Sends the page's Content-Security-Policy with the nonce Leptos puts on
/// this response's inline scripts. Responses that aren't pages get a stricter
/// one from the server.
#[cfg(feature = "ssr")]
fn send_page_policy() {
    let (Some(nonce), Some(response)) = (leptos::nonce::use_nonce(), use_context::<leptos_axum::ResponseOptions>()) else {
        return;
    };
    // `cargo leptos watch` reloads the page over a websocket to its own port.
    let watch = std::env::var("LEPTOS_WATCH").is_ok();
    if let Ok(value) = axum::http::HeaderValue::from_str(&page_policy(&nonce, watch)) {
        response.insert_header(axum::http::header::CONTENT_SECURITY_POLICY, value);
    }
}

/// What a page may load. Scripts run only with this response's nonce:
/// Leptos's hydration loader carries it, and `'strict-dynamic'` lets the code
/// it imports run, WebAssembly included. Styles come only from stylesheets:
/// colours are SVG fills (see [`paint`]), never `style` attributes. Under
/// `cargo leptos watch` the reload script injects styles and opens a websocket.
/// Pictures come from Discord's CDN.
#[cfg(feature = "ssr")]
fn page_policy(nonce: &str, watch: bool) -> String {
    let reload = if watch { " ws: wss:" } else { "" };
    let inline = if watch { " 'unsafe-inline'" } else { "" };
    [
        "default-src 'self'".to_string(),
        format!("script-src 'nonce-{nonce}' 'strict-dynamic' 'wasm-unsafe-eval'"),
        format!("style-src 'self'{inline} https://fonts.googleapis.com"),
        "font-src 'self' https://fonts.gstatic.com".to_string(),
        "img-src 'self' data: https://cdn.discordapp.com".to_string(),
        format!("connect-src 'self'{reload}"),
        "object-src 'none'".to_string(),
        "base-uri 'none'".to_string(),
        "form-action 'self'".to_string(),
        "frame-ancestors 'none'".to_string(),
    ]
    .join("; ")
}

#[cfg(all(test, feature = "ssr"))]
mod policy_tests {
    use super::page_policy;

    #[test]
    fn scripts_need_the_nonce_and_the_reload_socket_only_comes_with_watch() {
        let policy = page_policy("abc", false);
        assert!(policy.contains("script-src 'nonce-abc' 'strict-dynamic' 'wasm-unsafe-eval'"), "{policy}");
        assert!(!policy.contains("unsafe-inline' 'nonce") && !policy.contains("script-src 'self'"), "{policy}");
        assert!(policy.contains("connect-src 'self';"), "{policy}");
        assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
        assert!(policy.contains("style-src 'self' https://fonts.googleapis.com;"), "no inline styles: {policy}");
        let watching = page_policy("abc", true);
        assert!(watching.contains("connect-src 'self' ws: wss:;"));
        assert!(watching.contains("style-src 'self' 'unsafe-inline' "), "{watching}");
    }
}

#[derive(Clone, Copy)]
struct PageTitle(RwSignal<String>);

#[derive(Clone, Copy)]
pub(crate) struct ServerTitle(pub(crate) RwSignal<String>);

/// Sets the page half of the tab title. The shell adds the server name.
pub(crate) fn title(text: impl Into<String>) {
    if let Some(PageTitle(page)) = use_context() {
        page.set(text.into());
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    cache::provide();
    client::provide_toasts();
    // Effects run only in the browser, after hydration.
    Effect::new(|_| widgets::watch_presses());
    let page = RwSignal::new(String::new());
    let server = RwSignal::new(String::new());
    provide_context(PageTitle(page));
    provide_context(ServerTitle(server));
    let tab = move || {
        let page = page.get();
        let server = server.get();
        let body = match (page.is_empty(), server.is_empty()) {
            (true, true) => String::new(),
            (false, true) => page,
            (true, false) => server,
            (false, false) => format!("{page} · {server}"),
        };
        if body.is_empty() {
            "Marian".into()
        } else {
            format!("{body} · Marian")
        }
    };
    view! {
        <Meta name="description" content="Events for your Discord server."/>
        <Link rel="icon" href="/favicon.svg" type_="image/svg+xml"/>
        <Link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Fraunces:ital,opsz,wght@0,9..144,560;1,9..144,520&family=Source+Sans+3:wght@400;600;700&display=swap"/>
        <Router>
            <Routes fallback=pages::misc::NotFound>
                <Route path=path!("/") view=pages::misc::Front/>
                <Route path=path!("/help") view=pages::misc::Help/>
                <ParentRoute path=path!("/servers/:server") view=shell::ServerShell>
                    <Route path=path!("") view=pages::home::Schedule/>
                    <Route path=path!("events") view=pages::lists::AllEvents/>
                    <Route path=path!("events/new") view=pages::event::NewEvent/>
                    <Route path=path!("events/:id") view=pages::event::EventPage/>
                    <Route path=path!("events/:id/activity") view=pages::lists::EventActivity/>
                    <Route path=path!("upcoming") view=pages::lists::Upcoming/>
                    <Route path=path!("drafts") view=pages::lists::Drafts/>
                    <Route path=path!("scheduled") view=pages::lists::Scheduled/>
                    <Route path=path!("archive") view=pages::lists::Archive/>
                    <Route path=path!("activity") view=pages::lists::ActivityLog/>
                    <Route path=path!("recurring") view=pages::series::Recurring/>
                    <Route path=path!("recurring/:id") view=pages::series::SeriesPage/>
                    <Route path=path!("routines") view=pages::routines::Routines/>
                    <Route path=path!("routines/new") view=pages::routines::NewRoutine/>
                    <Route path=path!("routines/:id") view=pages::routines::RoutinePage/>
                    <Route path=path!("presets") view=pages::presets::Presets/>
                    <Route path=path!("channels") view=pages::channels::Channels/>
                    <Route path=path!("commands") view=pages::commands::Commands/>
                    <Route path=path!("settings") view=pages::settings::Settings/>
                    <Route path=path!("google") view=pages::settings::Google/>
                </ParentRoute>
            </Routes>
        </Router>
        <Title text=tab/>
    }
}

/// A path parameter of the current route.
fn param(name: &str) -> String {
    use_params_map().with_untracked(|params| params.get(name).unwrap_or_default())
}

/// A query parameter of the current address.
fn query(name: &str) -> String {
    use_query_map().with_untracked(|query| query.get(name).unwrap_or_default())
}

/// `/servers/{server}` for the server in the address.
fn base() -> String {
    format!("/servers/{}", param("server"))
}

pub(crate) fn failed(failure: Failure) -> impl IntoView {
    status(failure.status);
    if failure.missing() {
        return pages::misc::MissingHere().into_any();
    }
    view! { <p class="error">{failure.message}</p> }.into_any()
}

/// Sets the response status during server rendering.
fn status(code: u16) {
    #[cfg(feature = "ssr")]
    if let (Some(response), Ok(code)) = (
        use_context::<leptos_axum::ResponseOptions>(),
        axum::http::StatusCode::from_u16(code),
    ) {
        response.set_status(code);
    }
    #[cfg(not(feature = "ssr"))]
    let _ = code;
}

/// A colour as an SVG fill that fills whatever holds it: an event's stripe
/// down the left of its card or row (`class` "stripe"), or the face of a dot,
/// swatch or avatar ("paint"). SVG attributes aren't styles, so the
/// Content-Security-Policy needs no inline styles for colours of any value.
pub(crate) fn paint(class: &'static str, color: String) -> impl IntoView {
    view! {
        <svg class=class aria-hidden="true" focusable="false">
            <rect width="100%" height="100%" fill=color/>
        </svg>
    }
}

fn md(source: &str) -> impl IntoView {
    view! { <div class="md" inner_html=markdown::render(source)></div> }
}

fn head(heading: &'static str) -> impl IntoView {
    title(heading);
    view! {
        <div class="page-head"><div><h1>{heading}</h1></div></div>
    }
}
