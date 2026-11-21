//! The MCP server at `/mcp`: Streamable HTTP, answering each JSON-RPC
//! request with plain JSON. A client signs in through `oauth` and sends its
//! token as a bearer; every tool then runs as that person, through the same
//! checks the REST API makes.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use marian_types as dto;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::rest::{self, Problem};
use super::{AppState, Caller, read_session};
use crate::api;

/// The protocol versions Marian speaks, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "Marian schedules events in Discord servers and takes signups. \
Start with list_servers. Servers and events are named by the ids the tools return. \
Times you send are in the server's own time zone (get_server says which), as \
YYYY-MM-DDTHH:MM; times Marian sends back are UTC. New events are drafts unless you \
ask to post them: check the details with the person before posting, since a post \
goes out to the whole Discord channel.";

/// `GET` and `DELETE` on `/mcp`: Marian opens no stream of its own and keeps
/// no session.
pub(super) async fn not_here() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

pub(super) async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = bearer(&headers);
    let signed_in = match &token {
        Some(token) => match state.db.get().await {
            Ok(mut db) => read_session(&mut db, Caller::Mcp(token)).await.is_some(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        None => false,
    };
    let (Some(token), true) = (token, signed_in) else {
        return unauthorized(&state, &headers, bearer(&headers).is_some());
    };
    // Its servers stay current the way a browser's do.
    super::start_session_guilds_recheck(&state, &token).await;
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_reply(error(Value::Null, -32700, "That isn't JSON."));
    };
    let reply = match message {
        Value::Array(batch) => {
            let mut replies = Vec::new();
            for message in batch {
                if let Some(reply) = answer(&state, &token, message).await {
                    replies.push(reply);
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        message => answer(&state, &token, message).await,
    };
    match reply {
        Some(reply) => rpc_reply(reply),
        // Notifications and responses get no answer.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_string())
}

fn unauthorized(state: &AppState, headers: &HeaderMap, sent_token: bool) -> Response {
    let mut challenge = String::from("Bearer");
    if let Some(base) = super::oauth::public_base(state, headers) {
        challenge.push_str(&format!(
            " resource_metadata=\"{}\"",
            super::oauth::resource_metadata_url(&base)
        ));
    }
    if sent_token {
        challenge.push_str(", error=\"invalid_token\"");
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({ "error": "Connect Marian again: sign in with Discord." })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn rpc_reply(body: Value) -> Response {
    let mut response = axum::Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The answer to one message; none for a notification or a response.
async fn answer(state: &AppState, token: &str, message: Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str);
    let Some(id) = message.get("id").cloned() else {
        // A notification (or a response to nothing Marian asked): nothing to say.
        return None;
    };
    let Some(method) = method else {
        return Some(error(id, -32600, "Send a method."));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = VERSIONS
                .iter()
                .find(|version| **version == asked)
                .unwrap_or(&VERSIONS[0]);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "marian",
                        "title": "Marian",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call(state, token, name, args).await {
                Called::Done(value) => result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": value.to_string() }],
                        "structuredContent": value,
                        "isError": false,
                    }),
                ),
                Called::Failed(text) => result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": true,
                    }),
                ),
                Called::Unknown => error(id, -32602, &format!("Marian has no tool called {name}.")),
            }
        }
        _ => error(id, -32601, &format!("Marian doesn't answer {method}.")),
    })
}

enum Called {
    Done(Value),
    /// The tool ran and couldn't do it; the model reads why.
    Failed(String),
    Unknown,
}

impl From<Result<Value, String>> for Called {
    fn from(outcome: Result<Value, String>) -> Self {
        match outcome {
            Ok(value) => Called::Done(value),
            Err(text) => Called::Failed(text),
        }
    }
}

impl From<Problem> for String {
    fn from(problem: Problem) -> Self {
        problem.1
    }
}

/// A tool's arguments, or what's wrong with them.
fn args<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|err| format!("Check the arguments: {err}."))
}

fn to_value(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "Something went wrong.".to_string())
}

#[derive(Deserialize)]
struct ServerArgs {
    server: String,
}

#[derive(Deserialize)]
struct EventArgs {
    server: String,
    event: String,
}

async fn call(state: &AppState, token: &str, name: &str, raw: Value) -> Called {
    let caller = Caller::Mcp(token);
    if let Some(server) = raw.get("server").and_then(Value::as_str) {
        refresh_roles(state, token, server);
    }
    let outcome = match name {
        "list_servers" => list_servers(state, token).await,
        "get_server" => match args::<ServerArgs>(raw) {
            Ok(a) => get_server(state, caller, &a.server).await,
            Err(err) => Err(err),
        },
        "list_events" => list_events(state, caller, raw).await,
        "get_event" => match args::<EventArgs>(raw) {
            Ok(a) => get_event(state, caller, &a.server, &a.event).await,
            Err(err) => Err(err),
        },
        "create_event" => match args::<CreateArgs>(raw) {
            Ok(a) => create_event(state, caller, a).await,
            Err(err) => Err(err),
        },
        "update_event" => match args::<UpdateArgs>(raw) {
            Ok(a) => update_event(state, caller, a).await,
            Err(err) => Err(err),
        },
        "post_event" => match args::<EventArgs>(raw) {
            Ok(a) => rest::change_event(state, caller, &a.server, &a.event, async |db, v| {
                api::publish_event(db, v, &a.event).await
            })
            .await
            .map_err(String::from)
            .and_then(|event| to_value(event.0)),
            Err(err) => Err(err),
        },
        "delete_event" => match args::<EventArgs>(raw) {
            Ok(a) => rest::change(state, caller, &a.server, async |db, v| {
                api::delete_event(db, v, &a.event).await
            })
            .await
            .map(|_| json!({ "deleted": a.event }))
            .map_err(String::from),
            Err(err) => Err(err),
        },
        _ => return Called::Unknown,
    };
    outcome.into()
}

/// Brings the person's roles in this server up to date, as a page load does,
/// so what they may do follows Discord.
fn refresh_roles(state: &AppState, token: &str, server: &str) {
    let (state, token, path) = (
        state.clone(),
        token.to_string(),
        format!("/servers/{server}"),
    );
    tokio::spawn(async move {
        super::refresh_member_roles(&state, &path, Caller::Mcp(&token)).await;
    });
}

async fn list_servers(state: &AppState, token: &str) -> Result<Value, String> {
    let mut db = state
        .db
        .get()
        .await
        .map_err(|_| "The database is busy.".to_string())?;
    let viewer = read_session(&mut db, Caller::Mcp(token))
        .await
        .ok_or("Connect Marian again.")?;
    let me = api::me(&mut db, &viewer)
        .await
        .map_err(|err| String::from(Problem::from(err)))?;
    Ok(json!({ "you": me.name, "servers": me.servers }))
}

async fn get_server(state: &AppState, caller: Caller<'_>, server: &str) -> Result<Value, String> {
    let found = rest::read(state, caller, server, async |db, v| {
        let about = api::server(db, v).await?;
        let you = api::membership(db, v).await?;
        let channels = api::channels(db, v).await?;
        let presets = api::presets(db, v).await?;
        Ok(json!({
            "id": about.id,
            "name": about.name,
            "timezone": about.timezone,
            "you": {
                "name": you.name,
                "server_manager": you.server_manager,
                "can_create_events": you.can_create_event,
            },
            "event_counts": about.counts,
            "channels": channels
                .iter()
                .map(|channel| json!({ "id": channel.id, "name": channel.name }))
                .collect::<Vec<_>>(),
            "presets": presets
                .iter()
                .map(|preset| json!({
                    "id": preset.id,
                    "name": preset.name,
                    "default": preset.default,
                    "options": preset.options.iter().map(|option| &option.name).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(found.0)
}

async fn list_events(state: &AppState, caller: Caller<'_>, raw: Value) -> Result<Value, String> {
    let server: ServerArgs = args(raw.clone())?;
    let query: dto::EventQuery = args(raw)?;
    let page = rest::read(state, caller, &server.server, async |db, v| {
        api::events(db, v, &query).await
    })
    .await?;
    to_value(page.0)
}

async fn get_event(
    state: &AppState,
    caller: Caller<'_>,
    server: &str,
    event: &str,
) -> Result<Value, String> {
    let found = rest::read(state, caller, server, async |db, v| {
        let found = api::event(db, v, event).await?;
        let signups = api::signups(db, v, event).await?;
        Ok((found, signups))
    })
    .await?;
    let (event, signups) = found.0;
    Ok(json!({ "event": event, "signups": signups }))
}

/// A time in the server's zone, as people and models write them.
fn local_time(text: &str) -> Result<NaiveDateTime, String> {
    let text = text.trim();
    [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
    .ok_or_else(|| format!("Write {text} as YYYY-MM-DDTHH:MM, in the server's time zone."))
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Post {
    Draft,
    Now,
    At,
}

#[derive(Deserialize)]
struct CreateArgs {
    server: String,
    title: String,
    start: String,
    #[serde(default)]
    duration_minutes: Option<i64>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    reminder_minutes: Option<i64>,
    #[serde(default)]
    post: Option<Post>,
    #[serde(default)]
    post_at: Option<String>,
}

async fn create_event(
    state: &AppState,
    caller: Caller<'_>,
    a: CreateArgs,
) -> Result<Value, String> {
    let starts_local = local_time(&a.start)?;
    let (action, post_local) = match a.post.unwrap_or(Post::Draft) {
        Post::Draft => (dto::SaveAction::Draft, None),
        Post::Now => (dto::SaveAction::Publish, None),
        Post::At => {
            let at = a
                .post_at
                .as_deref()
                .ok_or("Say when to post it, in post_at.")?;
            (dto::SaveAction::Schedule, Some(local_time(at)?))
        }
    };
    let mut created = None;
    let _done = rest::change(state, caller, &a.server, async |db, v| {
        let channels = api::channels(db, v).await?;
        let channel = match a.channel.as_deref() {
            Some(wanted) => channels
                .iter()
                .find(|channel| channel.id == wanted || same_name(&channel.name, wanted))
                .ok_or_else(|| {
                    crate::error::OpError::message(format!(
                        "There's no channel {wanted} here. get_server lists them."
                    ))
                })?,
            None => channels.first().ok_or_else(|| {
                crate::error::OpError::message("This server has no channel for events yet.")
            })?,
        };
        let presets = api::presets(db, v).await?;
        let preset = match a.preset.as_deref() {
            Some(wanted) => Some(
                presets
                    .iter()
                    .find(|preset| preset.id == wanted || same_name(&preset.name, wanted))
                    .ok_or_else(|| {
                        crate::error::OpError::message(format!(
                            "There's no preset {wanted} here. get_server lists them."
                        ))
                    })?,
            ),
            None => presets
                .iter()
                .find(|preset| preset.default)
                .or_else(|| presets.first()),
        };
        let settings = api::settings(db, v).await?;
        let reminder = a
            .reminder_minutes
            .unwrap_or(settings.default_reminder_minutes);
        let draft = dto::EventDraft {
            title: a.title.clone(),
            description: a.description.clone(),
            location: a.location.clone(),
            channel_id: channel.id.clone(),
            color: "#1e3a6e".into(),
            starts_local,
            duration_minutes: a.duration_minutes.unwrap_or(60),
            cutoff_local: None,
            post_local,
            attendee_role_id: None,
            thread_enabled: settings.event_threads,
            multiple_signups: preset.is_some_and(|preset| preset.multiple_signups),
            mention_everyone: false,
            mention_role_ids: Vec::new(),
            restrict_role_ids: Vec::new(),
            options: preset
                .map(|preset| preset.options.clone())
                .unwrap_or_default(),
            reminders: (reminder > 0)
                .then(|| dto::Reminder {
                    offset_minutes: reminder,
                    message: String::new(),
                    sent: false,
                })
                .into_iter()
                .collect(),
            repeat: None,
            action,
            apply_series: false,
        };
        let outcome = api::create_event(db, v, &draft).await?;
        let id = outcome
            .done
            .location
            .as_deref()
            .and_then(|location| location.rsplit('/').next())
            .unwrap_or_default()
            .to_string();
        created = Some(api::event(db, v, &id).await?);
        Ok(outcome)
    })
    .await?;
    let event = created.ok_or("Something went wrong.")?;
    to_value(event)
}

fn same_name(name: &str, wanted: &str) -> bool {
    name.trim_start_matches('#')
        .eq_ignore_ascii_case(wanted.trim().trim_start_matches('#'))
}

#[derive(Deserialize)]
struct UpdateArgs {
    server: String,
    event: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    duration_minutes: Option<i64>,
    #[serde(default)]
    location: Option<String>,
}

async fn update_event(
    state: &AppState,
    caller: Caller<'_>,
    a: UpdateArgs,
) -> Result<Value, String> {
    let patch = dto::EventPatch {
        title: a.title,
        description: a.description,
        starts_local: a.start.as_deref().map(local_time).transpose()?,
        duration_minutes: a.duration_minutes,
        location: a.location,
        ..Default::default()
    };
    let event = rest::change_event(state, caller, &a.server, &a.event, async |db, v| {
        api::patch_event(db, v, &a.event, &patch).await
    })
    .await?;
    to_value(event.0)
}

fn tools() -> Value {
    let server = json!({ "type": "string", "description": "The server's id, from list_servers." });
    let event = json!({ "type": "string", "description": "The event's id, from list_events." });
    let local = |what: &str| {
        json!({
            "type": "string",
            "description": format!("{what}, in the server's time zone, as YYYY-MM-DDTHH:MM."),
        })
    };
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "list_servers",
            "title": "List servers",
            "description": "The Discord servers you share with Marian.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only,
        },
        {
            "name": "get_server",
            "title": "Get a server",
            "description": "A server's time zone, whether you may create events there, its event channels, and its signup presets.",
            "inputSchema": { "type": "object", "properties": { "server": server }, "required": ["server"] },
            "annotations": read_only,
        },
        {
            "name": "list_events",
            "title": "List events",
            "description": "A server's events, a page at a time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "view": {
                        "type": "string",
                        "enum": ["upcoming", "wrapped", "drafts", "queued", "archive", "all"],
                        "description": "upcoming (the default): posted events that haven't ended, and queued posts. wrapped: ended, not archived yet. drafts. queued: scheduled to post. archive. all.",
                    },
                    "when": { "type": "string", "enum": ["upcoming", "past"], "description": "Only events that haven't ended, or only ones that have." },
                    "q": { "type": "string", "description": "Words in the title." },
                    "from_day": { "type": "string", "description": "The first day an event may start on, YYYY-MM-DD, in the server's zone." },
                    "to_day": { "type": "string", "description": "The last day an event may start on, YYYY-MM-DD, in the server's zone." },
                    "page": { "type": "integer", "minimum": 1 },
                },
                "required": ["server"],
            },
            "annotations": read_only,
        },
        {
            "name": "get_event",
            "title": "Get an event",
            "description": "An event's details, and who signed up for what.",
            "inputSchema": { "type": "object", "properties": { "server": server, "event": event }, "required": ["server", "event"] },
            "annotations": read_only,
        },
        {
            "name": "create_event",
            "title": "Create an event",
            "description": "Creates an event, as a draft unless post says otherwise. Signup options come from a preset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "title": { "type": "string" },
                    "start": local("When it starts"),
                    "duration_minutes": { "type": "integer", "minimum": 1, "description": "60 if not given." },
                    "description": { "type": "string" },
                    "location": { "type": "string" },
                    "channel": { "type": "string", "description": "The channel's id or name, from get_server. The first one if not given." },
                    "preset": { "type": "string", "description": "The signup preset's id or name, from get_server. The server's default if not given." },
                    "reminder_minutes": { "type": "integer", "minimum": 0, "description": "Remind people this many minutes before; 0 for no reminder. The server's default if not given." },
                    "post": { "type": "string", "enum": ["draft", "now", "at"], "description": "draft (the default) keeps it in Marian; now posts it to Discord; at posts it at post_at." },
                    "post_at": local("When to post it, with post: at"),
                },
                "required": ["server", "title", "start"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true },
        },
        {
            "name": "update_event",
            "title": "Change an event",
            "description": "Changes an event's details; what isn't given stays. A posted event's Discord message follows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": server,
                    "event": event,
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "start": local("When it starts"),
                    "duration_minutes": { "type": "integer", "minimum": 1 },
                    "location": { "type": "string", "description": "Empty takes the location away." },
                },
                "required": ["server", "event"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        },
        {
            "name": "post_event",
            "title": "Post an event",
            "description": "Posts a draft or scheduled event to its Discord channel now.",
            "inputSchema": { "type": "object", "properties": { "server": server, "event": event }, "required": ["server", "event"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        },
        {
            "name": "delete_event",
            "title": "Delete an event",
            "description": "Deletes an event, with its Discord post and signups.",
            "inputSchema": { "type": "object", "properties": { "server": server, "event": event }, "required": ["server", "event"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_times_take_seconds_or_not() {
        assert_eq!(
            local_time("2026-11-25T19:00").unwrap(),
            local_time("2026-11-25 19:00:00").unwrap()
        );
        assert!(local_time("tomorrow").is_err());
    }

    #[test]
    fn every_tool_has_a_schema_and_unique_name() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let mut names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        assert!(
            tools
                .iter()
                .all(|tool| tool["inputSchema"]["type"] == "object")
        );
    }

    #[test]
    fn a_bearer_token_is_read_whatever_the_scheme_case() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer abc"),
        );
        assert_eq!(bearer(&headers).as_deref(), Some("abc"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&headers), None);
    }
}
