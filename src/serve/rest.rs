//! The REST API under `/api`: JSON resources, changed with PUT, PATCH, POST, and DELETE.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use marian_types as dto;
use serde::Deserialize;

use super::{clip, enqueue, events_to_refresh, read_session, AppState, Session};
use crate::api::{self, Outcome};
use crate::commands;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::logic;
use crate::store::Connection;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/client-errors", post(super::report::client_error))
        .route("/me", get(me))
        .route("/timezones", get(timezones))
        .route("/servers/{server}", get(server))
        .route("/servers/{server}/me", get(membership).patch(patch_membership))
        .route("/servers/{server}/settings", get(settings).put(put_settings))
        .route("/servers/{server}/roles", get(roles))
        .route("/servers/{server}/members", get(members))
        .route("/servers/{server}/members/search", get(search_members))
        .route("/servers/{server}/channels", get(channels).post(create_channel))
        .route("/servers/{server}/events", get(events).post(create_event))
        .route(
            "/servers/{server}/events/{id}",
            get(event).put(update_event).patch(patch_event).delete(delete_event),
        )
        .route("/servers/{server}/events/{id}/publish", post(publish_event))
        .route("/servers/{server}/events/{id}/schedule", post(schedule_event))
        .route("/servers/{server}/events/{id}/draft", post(unschedule_event))
        .route(
            "/servers/{server}/events/{id}/options",
            put(put_options),
        )
        .route("/servers/{server}/events/{id}/reminders", get(event_reminders))
        .route("/servers/{server}/events/{id}/signups", get(signups))
        .route(
            "/servers/{server}/events/{id}/signups/me/{option}",
            put(join).delete(leave),
        )
        .route(
            "/servers/{server}/events/{id}/signups/{member}/{option}",
            put(add_member).delete(remove_member),
        )
        .route("/servers/{server}/events/{id}/archive", post(archive))
        .route("/servers/{server}/events/{id}/duplicate", post(duplicate))
        .route("/servers/{server}/events/{id}/skip", post(skip))
        .route("/servers/{server}/live", get(live_server))
        .route("/servers/{server}/reminders", get(pending_reminders))
        .route("/servers/{server}/activity", get(activity))
        .route("/servers/{server}/series", get(series_list))
        .route("/servers/{server}/series/{id}", get(series).patch(patch_series))
        .route("/servers/{server}/series/{id}/end", post(end_series))
        .route("/servers/{server}/presets", get(presets).post(create_preset))
        .route("/servers/{server}/presets/{id}", get(preset).put(update_preset).delete(delete_preset))
        .route("/servers/{server}/presets/{id}/default", post(default_preset))
        .route("/servers/{server}/library", get(library).post(add_library))
        .route("/servers/{server}/library/{id}", axum::routing::patch(update_library).delete(delete_library))
        .route("/servers/{server}/events/{id}/preset", post(apply_preset))
        .route("/servers/{server}/commands", get(command_log).post(run_command))
        .route("/servers/{server}/routines", get(routines).post(create_routine))
        .route("/servers/{server}/routines/{id}", get(routine).put(update_routine))
        .route("/servers/{server}/routines/{id}/days", put(routine_days))
        .route("/servers/{server}/routines/{id}/vote", post(vote))
        .route("/servers/{server}/routines/{id}/stop", post(stop_routine))
        .fallback(missing)
}

pub(super) struct Problem(StatusCode, String);

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body = dto::Problem {
            error: self.1,
            redirect: None,
        };
        no_store((self.0, Json(body)).into_response())
    }
}

impl From<OpError> for Problem {
    fn from(err: OpError) -> Self {
        match err {
            OpError::NotFound => Problem(StatusCode::NOT_FOUND, "That isn't here.".into()),
            OpError::Message(text) => Problem(StatusCode::UNPROCESSABLE_ENTITY, text),
            OpError::Db(err) => {
                tracing::warn!("database: {err}");
                Problem(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.".into())
            }
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn missing() -> Problem {
    Problem(StatusCode::NOT_FOUND, "That isn't here.".into())
}

type Reply<T> = Result<Json<T>, Problem>;

/// Runs `body` for the signed-in person bound to `server`.
fn scoped<T>(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<T, OpError>,
) -> Result<T, Problem> {
    let mut db = state.db.get().map_err(|_| {
        Problem(StatusCode::SERVICE_UNAVAILABLE, "The database is busy.".into())
    })?;
    let Some(mut viewer) = read_session(&mut db, headers) else {
        return Err(Problem(StatusCode::UNAUTHORIZED, "Sign in with Discord.".into()));
    };
    api::bind_server(&mut db, &mut viewer, server).map_err(|err| match err {
        OpError::Message(text) => Problem(StatusCode::FORBIDDEN, text),
        other => other.into(),
    })?;
    Ok(body(&mut db, &viewer)?)
}

fn read<T>(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<T, OpError>,
) -> Reply<T> {
    scoped(state, headers, server, body).map(Json)
}

/// Runs a change, then hands its Discord work to the gateway. Open pages hear
/// about the event the change touched, or about the whole server.
fn change(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<Outcome, OpError>,
) -> Reply<dto::Done> {
    let mut guild_id = 0;
    let (outcome, event) = scoped(state, headers, server, |db, viewer| {
        guild_id = viewer.guild_id;
        let outcome = body(db, viewer)?;
        let event = outcome
            .sync
            .and_then(|id| crate::store::event(db, id).ok().flatten())
            .map(|event| event.public_id);
        Ok((outcome, event))
    })?;
    match event {
        Some(event) => crate::live::event(guild_id, event),
        None => crate::live::server(guild_id),
    }
    finish(state, outcome)
}

/// A change to who is signed up for one event.
fn change_signups(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    event: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<Outcome, OpError>,
) -> Reply<dto::Done> {
    let mut guild_id = 0;
    let outcome = scoped(state, headers, server, |db, viewer| {
        guild_id = viewer.guild_id;
        body(db, viewer)
    })?;
    match uuid::Uuid::parse_str(event) {
        Ok(event) => crate::live::signups(guild_id, event),
        Err(_) => crate::live::server(guild_id),
    }
    finish(state, outcome)
}

/// A personal write that other open pages do not need to refetch.
fn write(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<Outcome, OpError>,
) -> Reply<dto::Done> {
    finish(state, scoped(state, headers, server, body)?)
}

fn finish(state: &AppState, outcome: Outcome) -> Reply<dto::Done> {
    if let Some(event_id) = outcome.sync {
        enqueue(&state.discord, DiscordJob::sync(event_id));
        enqueue(&state.discord, DiscordJob::attend(event_id));
    }
    for job in outcome.jobs {
        enqueue(&state.discord, job);
    }
    Ok(Json(outcome.done))
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Reply<dto::Me> {
    let mut db = state
        .db
        .get()
        .map_err(|_| Problem(StatusCode::SERVICE_UNAVAILABLE, "The database is busy.".into()))?;
    let viewer = read_session(&mut db, &headers)
        .ok_or_else(|| Problem(StatusCode::UNAUTHORIZED, "Sign in with Discord.".into()))?;
    Ok(Json(api::me(&mut db, &viewer)?))
}

#[derive(Deserialize)]
struct ZoneQuery {
    #[serde(default)]
    selected: String,
}

async fn timezones(Query(query): Query<ZoneQuery>) -> Json<Vec<dto::ZoneGroup>> {
    Json(api::timezones(&query.selected))
}

type S = State<AppState>;
type One = Path<String>;
type Two = Path<(String, String)>;

async fn server(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<dto::Server> {
    read(&state, &headers, &server, api::server)
}

async fn membership(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<dto::Membership> {
    read(&state, &headers, &server, api::membership)
}

async fn patch_membership(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(patch): Json<dto::MembershipPatch>,
) -> Reply<dto::Done> {
    write(&state, &headers, &server, |db, v| api::patch_membership(db, v, &patch))
}

async fn settings(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<dto::Settings> {
    read(&state, &headers, &server, api::settings)
}

async fn put_settings(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(next): Json<dto::Settings>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::put_settings(db, v, &next))
}

async fn roles(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::Role>> {
    read(&state, &headers, &server, api::roles)
}

async fn members(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::Member>> {
    read(&state, &headers, &server, api::members)
}

#[derive(Deserialize)]
struct MemberQuery {
    #[serde(default)]
    q: String,
}

/// Everyone in the Discord server whose name starts with `q`, not just the
/// people Marian has met. Each one becomes a member here so they can be added.
async fn search_members(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Query(query): Query<MemberQuery>,
) -> Reply<Vec<dto::Member>> {
    scoped(&state, &headers, &server, |_, _| Ok(()))?;
    let needle = query.q.trim();
    if needle.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let found = discord_search(&state, &server, needle).await?;
    read(&state, &headers, &server, |db, v| api::remember_members(db, v, &found))
}

/// Asks Discord for up to 25 people in the server whose username or nickname
/// starts with `needle`. Bots are left out. Pairs are Discord id and name.
async fn discord_search(state: &AppState, server: &str, needle: &str) -> Result<Vec<(String, String)>, Problem> {
    #[derive(Deserialize)]
    struct User {
        id: String,
        username: String,
        #[serde(default)]
        global_name: Option<String>,
        #[serde(default)]
        bot: bool,
    }
    #[derive(Deserialize)]
    struct Found {
        user: User,
    }
    let unavailable = || Problem(StatusCode::BAD_GATEWAY, "Discord didn't answer the search. Try again.".into());
    let request = state
        .http
        .get(format!("https://discord.com/api/v10/guilds/{server}/members/search"))
        .query(&[("query", needle), ("limit", "25")])
        .header(header::AUTHORIZATION, format!("Bot {}", state.bot_token.trim()));
    let response = crate::observe::send_traced(request).await.map_err(|err| {
        tracing::warn!("Discord member search: {err}");
        unavailable()
    })?;
    if !response.status().is_success() {
        tracing::warn!("Discord member search returned {}", response.status());
        return Err(unavailable());
    }
    let found: Vec<Found> = response.json().await.map_err(|_| unavailable())?;
    Ok(found
        .into_iter()
        .filter(|found| !found.user.bot)
        .map(|found| {
            let name = crate::auth::discord_display_name(found.user.global_name.as_deref(), &found.user.username);
            (found.user.id, name)
        })
        .collect())
}

async fn channels(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::Channel>> {
    read(&state, &headers, &server, api::channels)
}

async fn create_channel(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(draft): Json<dto::ChannelDraft>,
) -> Reply<dto::Done> {
    let viewer = scoped(&state, &headers, &server, |_, v| Ok(v.clone()))?;
    open_channel(&state, &viewer, &draft.name).await?;
    crate::live::server(viewer.guild_id);
    Ok(Json(dto::Done { location: None }))
}

async fn open_channel(state: &AppState, viewer: &Session, name: &str) -> Result<String, Problem> {
    let http = poise::serenity_prelude::Http::new(&state.bot_token);
    crate::gateway::open_event_channel(&http, &state.db, viewer.guild_id, viewer.membership_id, name)
        .await
        .map_err(|err| Problem(StatusCode::UNPROCESSABLE_ENTITY, err.to_string()))
}

async fn events(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Query(query): Query<dto::EventQuery>,
) -> Reply<dto::Paged<dto::EventSummary>> {
    read(&state, &headers, &server, |db, v| api::events(db, v, &query))
}

async fn create_event(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(draft): Json<dto::EventDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::create_event(db, v, &draft))
}

async fn event(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Event> {
    read(&state, &headers, &server, |db, v| api::event(db, v, &id))
}

async fn update_event(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(draft): Json<dto::EventDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::update_event(db, v, &id, &draft))
}

/// Runs a change to one event and answers with the event as it now reads.
fn change_event(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    id: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<Outcome, OpError>,
) -> Reply<dto::Event> {
    let mut guild_id = 0;
    let (outcome, fresh) = scoped(state, headers, server, |db, v| {
        guild_id = v.guild_id;
        let outcome = body(db, v)?;
        Ok((outcome, api::event(db, v, id)?))
    })?;
    match uuid::Uuid::parse_str(&fresh.id) {
        Ok(event) => crate::live::event(guild_id, event),
        Err(_) => crate::live::server(guild_id),
    }
    let _ = finish(state, outcome)?;
    Ok(Json(fresh))
}

/// Changes details in place.
async fn patch_event(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(patch): Json<dto::EventPatch>,
) -> Reply<dto::Event> {
    change_event(&state, &headers, &server, &id, |db, v| api::patch_event(db, v, &id, &patch))
}

async fn publish_event(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Event> {
    change_event(&state, &headers, &server, &id, |db, v| api::publish_event(db, v, &id))
}

async fn unschedule_event(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Event> {
    change_event(&state, &headers, &server, &id, |db, v| api::unschedule_event(db, v, &id))
}

async fn schedule_event(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(when): Json<dto::PostAt>,
) -> Reply<dto::Event> {
    change_event(&state, &headers, &server, &id, |db, v| api::schedule_event(db, v, &id, &when))
}

async fn delete_event(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::delete_event(db, v, &id))
}

async fn put_options(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(options): Json<Vec<dto::SignupOption>>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::put_options(db, v, &id, &options))
}

async fn event_reminders(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
) -> Reply<Vec<dto::Reminder>> {
    read(&state, &headers, &server, |db, v| api::event_reminders(db, v, &id))
}


async fn signups(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Signups> {
    read(&state, &headers, &server, |db, v| api::signups(db, v, &id))
}

async fn join(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, option)): Path<(String, String, String)>,
) -> Reply<dto::Done> {
    change_signups(&state, &headers, &server, &id, |db, v| api::set_my_signup(db, v, &id, &option, true))
}

async fn leave(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, option)): Path<(String, String, String)>,
) -> Reply<dto::Done> {
    change_signups(&state, &headers, &server, &id, |db, v| api::set_my_signup(db, v, &id, &option, false))
}

async fn add_member(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, member, option)): Path<(String, String, String, String)>,
) -> Reply<dto::Done> {
    change_signups(&state, &headers, &server, &id, |db, v| {
        api::set_signup(db, v, &id, &member, &option, true)
    })
}

async fn remove_member(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, member, option)): Path<(String, String, String, String)>,
) -> Reply<dto::Done> {
    change_signups(&state, &headers, &server, &id, |db, v| {
        api::set_signup(db, v, &id, &member, &option, false)
    })
}

async fn archive(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::archive_event(db, v, &id))
}

async fn duplicate(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::duplicate_event(db, v, &id))
}

async fn skip(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::skip_event(db, v, &id))
}

/// Sends `changed` whenever this server changes, so open pages fetch again.
async fn live_server(State(state): S, headers: HeaderMap, Path(server): One) -> Result<Response, Problem> {
    let guild_id = scoped(&state, &headers, &server, |_, viewer| Ok(viewer.guild_id))?;
    Ok(live_sse(guild_id))
}

fn live_sse(guild_id: i64) -> Response {
    use tokio_stream::StreamExt as _;
    // The frontend this process serves, first on every connection. A page left
    // open across a deploy reconnects to the new process and learns it from here.
    let build = crate::assets::frontend_build();
    let first = (!build.is_empty()).then(|| {
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().event("build").data(build))
    });
    let changes = tokio_stream::wrappers::BroadcastStream::new(crate::live::subscribe())
        .filter_map(move |news| match news {
            Ok(news) if news.guild_id == guild_id => Some(Ok::<_, std::convert::Infallible>(
                axum::response::sse::Event::default().data(&*news.body),
            )),
            Ok(_) => None,
            Err(_) => Some(Ok(axum::response::sse::Event::default().data(crate::live::everything()))),
        });
    axum::response::sse::Sse::new(tokio_stream::iter(first).chain(changes))
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<i64>,
}

async fn pending_reminders(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Query(query): Query<Limit>,
) -> Reply<Vec<dto::PendingReminder>> {
    let limit = query.limit.unwrap_or(20).clamp(1, 200) as usize;
    read(&state, &headers, &server, |db, v| api::pending_reminders(db, v, limit))
}

#[derive(Deserialize)]
struct PageLimit {
    page: Option<u32>,
    limit: Option<i64>,
}

async fn activity(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Query(query): Query<PageLimit>,
) -> Reply<dto::Paged<dto::Activity>> {
    read(&state, &headers, &server, |db, v| {
        api::activity(db, v, query.page.unwrap_or(1), query.limit.unwrap_or(25))
    })
}

async fn series_list(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::Series>> {
    read(&state, &headers, &server, api::series_list)
}

async fn series(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Series> {
    read(&state, &headers, &server, |db, v| api::series(db, v, &id))
}

async fn patch_series(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(repeat): Json<dto::Repeat>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::patch_series(db, v, &id, &repeat))
}

async fn end_series(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::end_series(db, v, &id))
}

async fn presets(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::Preset>> {
    read(&state, &headers, &server, api::presets)
}

async fn preset(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Preset> {
    read(&state, &headers, &server, |db, v| api::preset(db, v, &id))
}

async fn create_preset(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(draft): Json<dto::PresetDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::create_preset(db, v, &draft))
}

async fn update_preset(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(draft): Json<dto::PresetDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::update_preset(db, v, &id, &draft))
}

async fn default_preset(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::default_preset(db, v, &id))
}

async fn delete_preset(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::delete_preset(db, v, &id))
}

async fn apply_preset(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(body): Json<dto::ApplyPreset>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::apply_preset(db, v, &id, &body))
}

async fn library(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::LibraryItem>> {
    read(&state, &headers, &server, api::library)
}

/// Answers with the new type, so a page can put it to use at once.
async fn add_library(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(draft): Json<dto::TypeDraft>,
) -> Reply<dto::LibraryItem> {
    let mut guild_id = 0;
    let made = scoped(&state, &headers, &server, |db, viewer| {
        guild_id = viewer.guild_id;
        api::add_library(db, viewer, &draft)
    })?;
    crate::live::server(guild_id);
    Ok(Json(made))
}

async fn update_library(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(draft): Json<dto::TypeDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::update_library(db, v, &id, &draft))
}

async fn delete_library(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::delete_library(db, v, &id))
}

async fn command_log(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::CommandLog>> {
    read(&state, &headers, &server, api::commands)
}

async fn run_command(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(run): Json<dto::CommandRun>,
) -> Reply<dto::CommandReply> {
    let viewer = scoped(&state, &headers, &server, |_, v| Ok(v.clone()))?;
    let line = run.line;
    if let Some(name) = commands::channel_request(&line) {
        let reply = match open_channel(&state, &viewer, &name).await {
            Ok(text) => text,
            Err(Problem(_, text)) => text,
        };
        if let Ok(mut db) = state.db.get() {
            let _ = logic::record_command(&mut db, viewer.guild_id, viewer.membership_id, &line, &reply);
        }
        crate::live::server(viewer.guild_id);
        return Ok(Json(dto::CommandReply {
            reply: clip(&reply, 700),
        }));
    }
    let (reply, ids) = {
        let mut db = state
            .db
            .get()
            .map_err(|_| Problem(StatusCode::SERVICE_UNAVAILABLE, "The database is busy.".into()))?;
        let reply = commands::execute(&mut db, viewer.guild_id, viewer.membership_id, &line);
        let ids = events_to_refresh(&mut db, &line, &reply);
        (reply, ids)
    };
    crate::live::server(viewer.guild_id);
    for id in ids {
        enqueue(&state.discord, DiscordJob::sync(id));
        enqueue(&state.discord, DiscordJob::attend(id));
    }
    Ok(Json(dto::CommandReply {
        reply: clip(&reply, 700),
    }))
}

async fn routines(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::RoutineSummary>> {
    read(&state, &headers, &server, api::routines)
}

async fn routine(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::RoutineView> {
    read(&state, &headers, &server, |db, v| api::routine(db, v, &id))
}

async fn create_routine(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(draft): Json<dto::RoutineDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::create_routine(db, v, &draft))
}

async fn update_routine(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(draft): Json<dto::RoutineDraft>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::update_routine(db, v, &id, &draft))
}

async fn routine_days(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(days): Json<dto::RoutineDays>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::routine_days(db, v, &id, &days))
}

async fn vote(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
    Json(vote): Json<dto::Vote>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::vote(db, v, &id, &vote))
}

async fn stop_routine(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::stop_routine(db, v, &id))
}
