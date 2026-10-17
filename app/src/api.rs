//! Each page and island asks for the resources it needs. On the server that reads
//! Marian directly; in the browser it calls the REST API.

use marian_types as dto;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub status: u16,
    pub message: String,
}

impl Failure {
    pub fn missing(&self) -> bool {
        self.status == 404
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

#[derive(Clone, Debug, Default)]
pub struct Site {
    pub invite_url: String,
    pub oauth_ready: bool,
    pub mark: String,
}

/// How this Marian is set up. Server rendering only.
pub fn site() -> Site {
    #[cfg(feature = "ssr")]
    if let Some(web) = leptos::prelude::use_context::<marian::api::Web>() {
        return Site {
            invite_url: web.invite_url,
            oauth_ready: web.oauth_ready,
            mark: marian::assets::mark().to_string(),
        };
    }
    Site::default()
}

/// The notice a sign-in or sign-out left, cleared once read. Server rendering only.
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
    String::new()
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
            send_wrapper::SendWrapper::new(remote::call::<_, ()>("GET", &url, None)).await
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
        send_wrapper::SendWrapper::new(remote::call::<_, ()>("GET", "/api/me", None)).await
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
        send_wrapper::SendWrapper::new(remote::call::<_, ()>("GET", &url, None)).await
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

pub async fn event_options(server: &str, id: &str) -> Result<Vec<dto::SignupOption>, Failure> {
    resource!(server, format!("/events/{id}/options"), |db, v| {
        marian::api::event_options(db, v, id)
    })
}

pub async fn event_reminders(server: &str, id: &str) -> Result<Vec<dto::Reminder>, Failure> {
    resource!(server, format!("/events/{id}/reminders"), |db, v| {
        marian::api::event_reminders(db, v, id)
    })
}

pub async fn thread(server: &str, id: &str) -> Result<dto::Thread, Failure> {
    resource!(server, format!("/events/{id}/thread"), |db, v| marian::api::thread(db, v, id))
}

pub async fn signups(server: &str, id: &str) -> Result<dto::Signups, Failure> {
    resource!(server, format!("/events/{id}/signups"), |db, v| marian::api::signups(db, v, id))
}

pub async fn pending_reminders(server: &str, limit: usize) -> Result<Vec<dto::PendingReminder>, Failure> {
    resource!(server, format!("/reminders?limit={limit}"), |db, v| {
        marian::api::pending_reminders(db, v, limit)
    })
}

pub async fn activity(server: &str, limit: i64) -> Result<Vec<dto::Activity>, Failure> {
    resource!(server, format!("/activity?limit={limit}"), |db, v| {
        marian::api::activity(db, v, limit)
    })
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

/// A change from an island. It only runs in the browser.
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
