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
        .route("/me", get(me))
        .route("/timezones", get(timezones))
        .route("/servers/{server}", get(server))
        .route("/servers/{server}/me", get(membership).patch(patch_membership))
        .route("/servers/{server}/settings", get(settings).put(put_settings))
        .route("/servers/{server}/roles", get(roles))
        .route("/servers/{server}/members", get(members))
        .route("/servers/{server}/channels", get(channels).post(create_channel))
        .route("/servers/{server}/events", get(events).post(create_event))
        .route(
            "/servers/{server}/events/{id}",
            get(event).put(update_event).delete(delete_event),
        )
        .route("/servers/{server}/events/{id}/options", get(event_options))
        .route("/servers/{server}/events/{id}/reminders", get(event_reminders))
        .route("/servers/{server}/events/{id}/thread", get(thread))
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
        .route("/servers/{server}/events/{id}/live", get(live))
        .route("/servers/{server}/reminders", get(pending_reminders))
        .route("/servers/{server}/activity", get(activity))
        .route("/servers/{server}/series", get(series_list))
        .route("/servers/{server}/series/{id}", get(series).patch(patch_series))
        .route("/servers/{server}/series/{id}/end", post(end_series))
        .route("/servers/{server}/presets", get(presets).post(create_preset))
        .route("/servers/{server}/presets/{id}", get(preset).delete(delete_preset))
        .route("/servers/{server}/library", get(library).post(add_library))
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

/// Runs a change, then hands its Discord work to the gateway.
fn change(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<Outcome, OpError>,
) -> Reply<dto::Done> {
    let outcome = scoped(state, headers, server, body)?;
    finish(state, outcome)
}

fn finish(state: &AppState, outcome: Outcome) -> Reply<dto::Done> {
    if let Some(event_id) = outcome.sync {
        crate::live::changed(event_id);
        enqueue(&state.discord, DiscordJob::sync(event_id));
        enqueue(&state.discord, DiscordJob::attend(event_id));
    }
    for job in outcome.jobs {
        enqueue(&state.discord, job);
    }
    let mut done = outcome.done;
    done.notice = clip(&done.notice, 700);
    Ok(Json(done))
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
    change(&state, &headers, &server, |db, v| api::patch_membership(db, v, &patch))
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
    let notice = open_channel(&state, &viewer, &draft.name).await?;
    Ok(Json(dto::Done {
        notice,
        location: None,
    }))
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

async fn delete_event(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::delete_event(db, v, &id))
}

async fn event_options(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
) -> Reply<Vec<dto::SignupOption>> {
    read(&state, &headers, &server, |db, v| api::event_options(db, v, &id))
}

async fn event_reminders(
    State(state): S,
    headers: HeaderMap,
    Path((server, id)): Two,
) -> Reply<Vec<dto::Reminder>> {
    read(&state, &headers, &server, |db, v| api::event_reminders(db, v, &id))
}

async fn thread(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Thread> {
    read(&state, &headers, &server, |db, v| api::thread(db, v, &id))
}

async fn signups(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Signups> {
    read(&state, &headers, &server, |db, v| api::signups(db, v, &id))
}

async fn join(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, option)): Path<(String, String, i64)>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::set_my_signup(db, v, &id, option, true))
}

async fn leave(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, option)): Path<(String, String, i64)>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::set_my_signup(db, v, &id, option, false))
}

async fn add_member(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, member, option)): Path<(String, String, i64, i64)>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| {
        api::set_signup(db, v, &id, member, option, true)
    })
}

async fn remove_member(
    State(state): S,
    headers: HeaderMap,
    Path((server, id, member, option)): Path<(String, String, i64, i64)>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| {
        api::set_signup(db, v, &id, member, option, false)
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

/// Sends `changed` whenever the event changes, so open pages fetch it again.
async fn live(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Result<Response, Problem> {
    let event_id = scoped(&state, &headers, &server, |db, v| api::event_key(db, v, &id))?;
    use tokio_stream::StreamExt as _;
    let changes = tokio_stream::wrappers::BroadcastStream::new(crate::live::subscribe())
        .filter_map(move |change| match change {
            Ok(id) if id == event_id => Some(Ok::<_, std::convert::Infallible>(
                axum::response::sse::Event::default().data("changed"),
            )),
            Ok(_) => None,
            Err(_) => Some(Ok(axum::response::sse::Event::default().data("changed"))),
        });
    Ok(axum::response::sse::Sse::new(changes)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
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

async fn activity(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Query(query): Query<Limit>,
) -> Reply<Vec<dto::Activity>> {
    read(&state, &headers, &server, |db, v| api::activity(db, v, query.limit.unwrap_or(80)))
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

async fn delete_preset(State(state): S, headers: HeaderMap, Path((server, id)): Two) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::delete_preset(db, v, &id))
}

async fn library(State(state): S, headers: HeaderMap, Path(server): One) -> Reply<Vec<dto::LibraryItem>> {
    read(&state, &headers, &server, api::library)
}

async fn add_library(
    State(state): S,
    headers: HeaderMap,
    Path(server): One,
    Json(item): Json<dto::LibraryItem>,
) -> Reply<dto::Done> {
    change(&state, &headers, &server, |db, v| api::add_library(db, v, &item))
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
    for id in ids {
        crate::live::changed(id);
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
