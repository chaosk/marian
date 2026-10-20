//! Each page asks for the resources it needs. On the server that reads
//! Marian directly; in the browser it calls the REST API.

use marian_types as dto;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A read that failed is an error a view can return, so an `ErrorBoundary`
/// around the page shows it.
impl std::error::Error for Failure {}

impl Failure {
    pub fn missing(&self) -> bool {
        self.status == 404
    }

    pub fn unreachable(&self) -> bool {
        self.status == 0
    }
}

#[cfg(feature = "ssr")]
pub(crate) mod local {
    use axum::http::request::Parts;
    use leptos::prelude::use_context;
    use marian::api::{self, Viewer, Web};
    use marian::store::Connection;
    use marian::OpError;

    use super::Failure;

    fn failure(err: OpError) -> Failure {
        match err {
            OpError::NotFound => Failure {
                status: 404,
                message: "That isn't here.".into(),
            },
            OpError::Message(message) => Failure {
                status: 403,
                message,
            },
            OpError::Db(_) => Failure {
                status: 500,
                message: "Something went wrong.".into(),
            },
        }
    }

    fn unsigned() -> Failure {
        Failure {
            status: 401,
            message: "Sign in with Discord.".into(),
        }
    }

    pub fn web() -> Option<(Web, Parts)> {
        Some((use_context::<Web>()?, use_context::<Parts>()?))
    }

    pub fn signed_in<T>(
        body: impl FnOnce(&mut Connection, &Viewer) -> Result<T, OpError>,
    ) -> Result<T, Failure> {
        let (web, parts) = web().ok_or_else(unsigned)?;
        let mut db = web.db.get().map_err(|_| Failure {
            status: 503,
            message: "The database is busy.".into(),
        })?;
        let viewer = api::viewer(&mut db, &parts.headers).ok_or_else(unsigned)?;
        body(&mut db, &viewer).map_err(failure)
    }

    pub fn with<T>(
        server: &str,
        body: impl FnOnce(&mut Connection, &Viewer) -> Result<T, OpError>,
    ) -> Result<T, Failure> {
        signed_in(|db, viewer| {
            let mut viewer = viewer.clone();
            api::bind_server(db, &mut viewer, server)?;
            body(db, &viewer)
        })
    }
}

#[cfg(not(feature = "ssr"))]
pub(crate) mod remote {
    use serde::de::DeserializeOwned;
    use serde::Serialize;

    use super::Failure;

    fn offline() -> Failure {
        Failure {
            status: 0,
            message: "Marian can't be reached. Check your connection.".into(),
        }
    }

    const READ_TRIES: u32 = 4;

    pub async fn read<T: DeserializeOwned>(url: &str) -> Result<T, Failure> {
        let mut last = offline();
        for attempt in 0..READ_TRIES {
            match call::<T, ()>("GET", url, None).await {
                Ok(value) => {
                    crate::client::note_reached();
                    return Ok(value);
                }
                Err(failure) if failure.unreachable() && attempt + 1 < READ_TRIES => {
                    crate::client::set_waiting(true);
                    last = failure;
                    pause(400 << attempt).await;
                }
                Err(failure) => {
                    if failure.unreachable() {
                        crate::client::say_offline(&failure.message);
                    } else {
                        crate::client::set_waiting(false);
                    }
                    return Err(failure);
                }
            }
        }
        crate::client::say_offline(&last.message);
        Err(last)
    }

    async fn pause(ms: i32) {
        #[cfg(feature = "hydrate")]
        {
            use wasm_bindgen::prelude::Closure;
            use wasm_bindgen::JsCast;
            let promise = js_sys::Promise::new(&mut |resolve, _| {
                let cb = Closure::once_into_js(move || {
                    let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
                });
                let _ = leptos::prelude::window()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms);
            });
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        }
        #[cfg(not(feature = "hydrate"))]
        {
            let _ = ms;
        }
    }

    pub async fn call<T: DeserializeOwned, B: Serialize>(
        method: &str,
        url: &str,
        body: Option<&B>,
    ) -> Result<T, Failure> {
        #[cfg(feature = "hydrate")]
        {
            use gloo_net::http::{Method, RequestBuilder};
            let method = match method {
                "POST" => Method::POST,
                "PUT" => Method::PUT,
                "PATCH" => Method::PATCH,
                "DELETE" => Method::DELETE,
                _ => Method::GET,
            };
            let builder = RequestBuilder::new(url)
                .method(method)
                .header("Accept", "application/json");
            let request = match body {
                Some(body) => builder.json(body).map_err(|_| offline())?,
                None => builder.build().map_err(|_| offline())?,
            };
            let response = request.send().await.map_err(|_| offline())?;
            let status = response.status();
            if (200..300).contains(&status) {
                return response.json::<T>().await.map_err(|_| offline());
            }
            let problem = response.json::<marian_types::Problem>().await.ok();
            if status == 401 {
                if let Some(to) = problem.as_ref().and_then(|p| p.redirect.clone()) {
                    let _ = leptos::prelude::window().location().assign(&to);
                }
            }
            Err(Failure {
                status,
                message: problem
                    .map(|p| p.error)
                    .unwrap_or_else(|| "Something went wrong.".into()),
            })
        }
        #[cfg(not(feature = "hydrate"))]
        {
            let _ = (method, url, body);
            Err(offline())
        }
    }
}

/// What the front page shows someone.
pub enum Front {
    Home(String),
    Landing { ready: bool, next: String },
}

/// The signed-in person's home server, or the landing page. Server rendering only.
pub fn front(next: &str) -> Front {
    #[cfg(feature = "ssr")]
    if let Ok(Some(server)) = local::signed_in(|db, v| Ok(marian::api::home_server(db, v))) {
        return Front::Home(server);
    }
    Front::Landing {
        ready: site().oauth_ready,
        next: next.to_string(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Site {
    pub invite_url: String,
    pub oauth_ready: bool,
    pub mark: String,
}

/// How this Marian is set up. The shell writes this into the document so
/// hydrate builds the same tree the server sent.
pub fn site() -> Site {
    #[cfg(feature = "ssr")]
    if let Some(web) = leptos::prelude::use_context::<marian::api::Web>() {
        return Site {
            invite_url: web.invite_url,
            oauth_ready: web.oauth_ready,
            mark: marian::assets::mark().to_string(),
        };
    }
    #[cfg(feature = "hydrate")]
    {
        return site_from_document();
    }
    #[allow(unreachable_code)]
    Site::default()
}

/// The trace this page was rendered in, for the browser's error reports.
pub fn page_trace() -> Option<String> {
    #[cfg(feature = "ssr")]
    {
        use leptos::prelude::use_context;
        return use_context::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<marian::api::PageTrace>().cloned())
            .map(|trace| trace.0);
    }
    #[allow(unreachable_code)]
    None
}

/// JSON for the `<script id="marian-site">` the shell ships to the browser.
pub fn site_json() -> String {
    serde_json::to_string(&site())
        .unwrap_or_else(|_| "{}".into())
        .replace('<', "\\u003c")
}

#[cfg(feature = "hydrate")]
fn site_from_document() -> Site {
    thread_local! {
        static CACHED: std::cell::OnceCell<Site> = const { std::cell::OnceCell::new() };
    }
    CACHED.with(|cell| {
        cell.get_or_init(|| {
            leptos::prelude::document()
                .get_element_by_id("marian-site")
                .and_then(|el| el.text_content())
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default()
        })
        .clone()
    })
}

/// The notice a sign-in or sign-out left, cleared once read.
pub fn take_notice() -> String {
    #[cfg(feature = "ssr")]
    if let Some((_, parts)) = local::web() {
        if let Some(raw) = marian::api::cookie(&parts.headers, "marian_notice") {
            if let Some(response) = leptos::prelude::use_context::<leptos_axum::ResponseOptions>() {
                response.append_header(
                    axum::http::header::SET_COOKIE,
                    axum::http::HeaderValue::from_static(
                        "marian_notice=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
                    ),
                );
            }
            return marian::api::percent_decode(&raw);
        }
    }
    #[cfg(feature = "hydrate")]
    {
        notice_from_document()
    }
    #[cfg(not(feature = "hydrate"))]
    String::new()
}

/// JSON for the `<script id="marian-notice">` the shell ships to the browser.
pub fn notice_json() -> String {
    serde_json::to_string(&take_notice())
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c")
}

#[cfg(feature = "hydrate")]
fn notice_from_document() -> String {
    thread_local! {
        static CACHED: std::cell::OnceCell<String> = const { std::cell::OnceCell::new() };
    }
    CACHED.with(|cell| {
        cell.get_or_init(|| {
            leptos::prelude::document()
                .get_element_by_id("marian-notice")
                .and_then(|el| el.text_content())
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default()
        })
        .clone()
    })
}

/// Marian's own call on the server; the REST resource at `$url` in the browser.
macro_rules! resource {
    ($server:expr, $url:expr, $body:expr) => {{
        #[cfg(feature = "ssr")]
        {
            local::with($server, $body)
        }
        #[cfg(not(feature = "ssr"))]
        {
            let url = format!("/api/servers/{}{}", $server, $url);
            send_wrapper::SendWrapper::new(remote::read::<_>(&url)).await
        }
    }};
}

pub async fn me() -> Result<dto::Me, Failure> {
    #[cfg(feature = "ssr")]
    {
        local::signed_in(marian::api::me)
    }
    #[cfg(not(feature = "ssr"))]
    {
        send_wrapper::SendWrapper::new(remote::read::<_>("/api/me")).await
    }
}

pub async fn server(server: &str) -> Result<dto::Server, Failure> {
    resource!(server, "", marian::api::server)
}

pub async fn membership(server: &str) -> Result<dto::Membership, Failure> {
    resource!(server, "/me", marian::api::membership)
}

pub async fn settings(server: &str) -> Result<dto::Settings, Failure> {
    resource!(server, "/settings", marian::api::settings)
}

pub async fn timezones(selected: &str) -> Result<Vec<dto::ZoneGroup>, Failure> {
    #[cfg(feature = "ssr")]
    {
        Ok(marian::api::timezones(selected))
    }
    #[cfg(not(feature = "ssr"))]
    {
        let url = format!("/api/timezones?selected={}", encode(selected));
        send_wrapper::SendWrapper::new(remote::read::<_>(&url)).await
    }
}

pub async fn roles(server: &str) -> Result<Vec<dto::Role>, Failure> {
    resource!(server, "/roles", marian::api::roles)
}

pub async fn members(server: &str) -> Result<Vec<dto::Member>, Failure> {
    resource!(server, "/members", marian::api::members)
}

pub async fn channels(server: &str) -> Result<Vec<dto::Channel>, Failure> {
    resource!(server, "/channels", marian::api::channels)
}

pub async fn events(server: &str, query: &dto::EventQuery) -> Result<dto::Paged<dto::EventSummary>, Failure> {
    resource!(server, format!("/events{}", query_string(query)), |db, v| {
        marian::api::events(db, v, query)
    })
}

pub async fn event(server: &str, id: &str) -> Result<dto::Event, Failure> {
    resource!(server, format!("/events/{id}"), |db, v| marian::api::event(db, v, id))
}

pub async fn event_reminders(server: &str, id: &str) -> Result<Vec<dto::Reminder>, Failure> {
    resource!(server, format!("/events/{id}/reminders"), |db, v| {
        marian::api::event_reminders(db, v, id)
    })
}


pub async fn signups(server: &str, id: &str) -> Result<dto::Signups, Failure> {
    resource!(server, format!("/events/{id}/signups"), |db, v| marian::api::signups(db, v, id))
}

pub async fn pending_reminders(server: &str, limit: usize) -> Result<Vec<dto::PendingReminder>, Failure> {
    resource!(server, format!("/reminders?limit={limit}"), |db, v| {
        marian::api::pending_reminders(db, v, limit)
    })
}

pub async fn activity(server: &str, page: u32, limit: u32) -> Result<dto::Paged<dto::Activity>, Failure> {
    resource!(
        server,
        format!("/activity?page={page}&limit={limit}"),
        |db, v| { marian::api::activity(db, v, page, i64::from(limit)) }
    )
}

pub async fn series_list(server: &str) -> Result<Vec<dto::Series>, Failure> {
    resource!(server, "/series", marian::api::series_list)
}

pub async fn series(server: &str, id: &str) -> Result<dto::Series, Failure> {
    resource!(server, format!("/series/{id}"), |db, v| marian::api::series(db, v, id))
}

pub async fn presets(server: &str) -> Result<Vec<dto::Preset>, Failure> {
    resource!(server, "/presets", marian::api::presets)
}

pub async fn preset(server: &str, id: &str) -> Result<dto::Preset, Failure> {
    resource!(server, format!("/presets/{id}"), |db, v| marian::api::preset(db, v, id))
}

pub async fn library(server: &str) -> Result<Vec<dto::LibraryItem>, Failure> {
    resource!(server, "/library", marian::api::library)
}

pub async fn commands(server: &str) -> Result<Vec<dto::CommandLog>, Failure> {
    resource!(server, "/commands", marian::api::commands)
}

pub async fn routines(server: &str) -> Result<Vec<dto::RoutineSummary>, Failure> {
    resource!(server, "/routines", marian::api::routines)
}

pub async fn routine(server: &str, id: &str) -> Result<dto::RoutineView, Failure> {
    resource!(server, format!("/routines/{id}"), |db, v| marian::api::routine(db, v, id))
}

/// A change from the browser. It only runs there.
pub async fn send<T: for<'de> Deserialize<'de>, B: Serialize>(
    method: &str,
    url: &str,
    body: Option<&B>,
) -> Result<T, Failure> {
    #[cfg(not(feature = "ssr"))]
    {
        remote::call(method, url, body).await
    }
    #[cfg(feature = "ssr")]
    {
        let _ = (method, url, body);
        Err(Failure {
            status: 0,
            message: "Changes are sent from the browser.".into(),
        })
    }
}

pub fn query_string<T: Serialize>(value: &T) -> String {
    let pairs = serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (key, value) in pairs {
        let text = match value {
            serde_json::Value::Null => continue,
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        };
        out.push(format!("{key}={}", encode(&text)));
    }
    if out.is_empty() {
        String::new()
    } else {
        format!("?{}", out.join("&"))
    }
}

pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_queries_skip_what_is_unset() {
        let query = dto::EventQuery {
            view: dto::EventView::Drafts,
            q: "game night".into(),
            page: Some(2),
            ..Default::default()
        };
        assert_eq!(query_string(&query), "?page=2&q=game%20night&view=drafts");
    }
}
