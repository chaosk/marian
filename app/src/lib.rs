//! Marian's web pages, rendered on the server, with islands that run in the browser.

#![recursion_limit = "256"]

use std::future::Future;

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet};
use leptos_router::components::{ParentRoute, Route, Router, Routes};
use leptos_router::hooks::{use_params_map, use_query_map};
use leptos_router::{path, SsrMode};

pub mod api;
mod client;
mod format;
mod islands;
mod markdown;
mod pages;
mod shell;

use api::Failure;

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <link rel="icon" href="/favicon.svg" type="image/svg+xml"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options islands=true/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    view! {
        <Stylesheet id="marian" href="/pkg/marian.css"/>
        <Router>
            <Routes fallback=pages::misc::NotFound>
                <Route path=path!("/") view=pages::misc::Front ssr=SsrMode::Async/>
                <Route path=path!("/help") view=pages::misc::Help ssr=SsrMode::Async/>
                <ParentRoute path=path!("/servers/:server") view=shell::ServerShell ssr=SsrMode::Async>
                    <Route path=path!("") view=pages::home::Schedule ssr=SsrMode::Async/>
                    <Route path=path!("events/new") view=pages::form::NewEvent ssr=SsrMode::Async/>
                    <Route path=path!("events/:id") view=pages::event::EventPage ssr=SsrMode::Async/>
                    <Route path=path!("events/:id/edit") view=pages::form::EditEvent ssr=SsrMode::Async/>
                    <Route path=path!("drafts") view=pages::lists::Drafts ssr=SsrMode::Async/>
                    <Route path=path!("scheduled") view=pages::lists::Scheduled ssr=SsrMode::Async/>
                    <Route path=path!("archive") view=pages::lists::Archive ssr=SsrMode::Async/>
                    <Route path=path!("activity") view=pages::lists::ActivityLog ssr=SsrMode::Async/>
                    <Route path=path!("recurring") view=pages::series::Recurring ssr=SsrMode::Async/>
                    <Route path=path!("recurring/:id") view=pages::series::SeriesPage ssr=SsrMode::Async/>
                    <Route path=path!("routines") view=pages::routines::Routines ssr=SsrMode::Async/>
                    <Route path=path!("routines/new") view=pages::routines::NewRoutine ssr=SsrMode::Async/>
                    <Route path=path!("routines/:id") view=pages::routines::RoutinePage ssr=SsrMode::Async/>
                    <Route path=path!("presets") view=pages::presets::Presets ssr=SsrMode::Async/>
                    <Route path=path!("channels") view=pages::channels::Channels ssr=SsrMode::Async/>
                    <Route path=path!("commands") view=pages::commands::Commands ssr=SsrMode::Async/>
                    <Route path=path!("settings") view=pages::settings::Settings ssr=SsrMode::Async/>
                    <Route path=path!("google") view=pages::settings::Google ssr=SsrMode::Async/>
                </ParentRoute>
            </Routes>
        </Router>
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

/// Renders `render` once `data` arrives, or explains why it didn't.
fn load<T, V>(
    data: impl Future<Output = Result<T, Failure>> + Send + 'static,
    render: impl FnOnce(T) -> V + Send + 'static,
) -> impl IntoView
where
    T: Send + 'static,
    V: IntoView + 'static,
{
    view! {
        <Suspense fallback=|| ()>
            {Suspend::new(async move {
                match data.await {
                    Ok(value) => render(value).into_any(),
                    Err(failure) => failed(failure).into_any(),
                }
            })}
        </Suspense>
    }
}

fn failed(failure: Failure) -> impl IntoView {
    status(failure.status);
    if failure.missing() {
        return pages::misc::NotFound().into_any();
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

/// Sends the browser elsewhere during server rendering.
fn redirect(to: &str) {
    #[cfg(feature = "ssr")]
    leptos_axum::redirect(to);
    #[cfg(not(feature = "ssr"))]
    let _ = to;
}

fn border(color: &str) -> String {
    format!("border-left-color: {color}")
}

fn md(source: &str) -> impl IntoView {
    view! { <div class="md" inner_html=markdown::render(source)></div> }
}

fn head(title: &'static str) -> impl IntoView {
    view! { <div class="page-head"><div><h1>{title}</h1></div></div> }
}
