use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use rusqlite::Connection;
use tokio::sync::mpsc::UnboundedSender;

use crate::auth;
use crate::clock;
use crate::commands;
use crate::error::OpError;
use crate::ics;
use crate::jobs::DiscordJob;
use crate::logic::{self, OptionInput, PresetBody};
use crate::present::{self, Page};
use crate::store::{self, Settings};

const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#14284d"/><path d="M8 22V10h3.2l4.8 7.2L20.8 10H24v12h-2.6v-7.1L17.2 22h-2.4l-4.2-7.1V22H8z" fill="#f4efe4"/></svg>"##;

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    discord: UnboundedSender<DiscordJob>,
    invite_url: String,
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    public_url: String,
}

struct Session {
    discord_user_id: String,
    display_name: String,
    guild_id: i64,
    user_id: i64,
    allowed: Vec<i64>,
    notice: Option<String>,
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "marian=info,tower_http=info".into()),
        )
        .init();

    let db_path = env::var("MARIAN_DB").unwrap_or_else(|_| "data/marian.db".into());
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080);
    let token = env::var("DISCORD_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        anyhow::bail!("Set DISCORD_TOKEN. Marian does not start without a bot token.");
    }
    let client_id = env::var("DISCORD_CLIENT_ID").unwrap_or_default();
    let client_secret = env::var("DISCORD_CLIENT_SECRET").unwrap_or_default();
    let public_url = env::var("MARIAN_PUBLIC_URL").unwrap_or_default();
    let status = env::var("MARIAN_STATUS").unwrap_or_default();
    let invite_url = invite_url(&client_id);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut conn = store::open(&PathBuf::from(&db_path))?;
    logic::rebase(&mut conn, Utc::now())?;
    let db = Arc::new(Mutex::new(conn));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState {
        db,
        discord: tx.clone(),
        invite_url,
        http,
        client_id,
        client_secret,
        public_url: public_url.clone(),
    };
    let app = router(state.clone());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        crate::gateway::start(token, state.db.clone(), rx, tx, public_url, status);
        let db = state.db.clone();
        let discord = state.discord.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let report = {
                    let mut guard = db.lock().expect("database lock");
                    logic::tick(&mut guard, Utc::now())
                };
                match report {
                    Ok(report) => {
                        for note in report.notes {
                            tracing::info!("{note}");
                        }
                        for job in report.jobs {
                            enqueue(&discord, job);
                        }
                    }
                    Err(err) => tracing::warn!("tick failed: {err}"),
                }
            }
        });

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Marian is listening on {addr}");
        axum::serve(listener, app).await?;
        Ok(())
    })
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/favicon.svg", get(favicon))
        .route("/static/app.css", get(css))
        .route("/static/app.js", get(js))
        .route("/static/marian.png", get(portrait))
        .route("/", get(schedule))
        .route("/auth/discord", get(auth_start))
        .route("/auth/discord/callback", get(auth_callback))
        .route("/logout", post(logout))
        .route("/servers/:guild_id", get(server_home))
        .route(
            "/servers/:guild_id/events/new",
            get(new_event).post(create_event),
        )
        .route("/servers/:guild_id/events/:id", get(show_event))
        .route(
            "/servers/:guild_id/events/:id/edit",
            get(edit_event).post(save_event),
        )
        .route("/servers/:guild_id/events/:id/signup", post(signup))
        .route("/servers/:guild_id/events/:id/response", post(response))
        .route("/servers/:guild_id/events/:id/archive", post(archive))
        .route("/servers/:guild_id/events/:id/delete", post(delete_event))
        .route("/servers/:guild_id/events/:id/duplicate", post(duplicate))
        .route("/servers/:guild_id/events/:id/skip", post(skip))
        .route("/servers/:guild_id/drafts", get(drafts))
        .route("/servers/:guild_id/scheduled", get(scheduled))
        .route("/servers/:guild_id/recurring", get(recurring))
        .route("/servers/:guild_id/recurring/:id", get(series))
        .route("/servers/:guild_id/recurring/:id/end", post(end_series))
        .route("/servers/:guild_id/recurring/:id/vote", post(series_vote))
        .route("/servers/:guild_id/archive", get(archive_list))
        .route(
            "/servers/:guild_id/presets",
            get(presets_page).post(save_preset),
        )
        .route("/servers/:guild_id/presets/library", post(save_library))
        .route("/servers/:guild_id/presets/:id/delete", post(delete_preset))
        .route(
            "/servers/:guild_id/channels",
            get(channels_page).post(create_channel),
        )
        .route(
            "/servers/:guild_id/commands",
            get(commands_page).post(run_command),
        )
        .route("/servers/:guild_id/activity", get(activity))
        .route(
            "/servers/:guild_id/settings",
            get(settings_page).post(save_settings),
        )
        .route("/servers/:guild_id/me/reminders", post(toggle_reminders))
        .route("/servers/:guild_id/calendar/:file", get(event_calendar))
        .route("/drafts", get(legacy_drafts))
        .route("/scheduled", get(legacy_scheduled))
        .route("/recurring", get(legacy_recurring))
        .route("/archive", get(legacy_archive))
        .route("/presets", get(legacy_presets))
        .route("/channels", get(legacy_channels))
        .route("/commands", get(legacy_commands))
        .route("/activity", get(legacy_activity))
        .route("/settings", get(legacy_settings))
        .route("/events/:id", get(legacy_event))
        .route("/events/:id/edit", get(legacy_edit))
        .route("/calendar/:file", get(legacy_event_calendar))
        .layer(middleware::from_fn_with_state(state.clone(), require_login))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn ready(State(state): State<AppState>) -> Response {
    let ok = state.db.lock().map(|conn| {
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_ok()
    });
    if ok.unwrap_or(false) {
        (StatusCode::OK, "ok").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
    }
}

async fn favicon() -> Response {
    ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON).into_response()
}

async fn css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/app.css"),
    )
        .into_response()
}

async fn js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/app.js"),
    )
        .into_response()
}

async fn portrait() -> Response {
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("../static/marian.png").as_slice(),
    )
        .into_response()
}

async fn schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Response {
    let notice = cookie(&headers, "marian_notice").map(|value| percent_decode(&value));
    let signed_in = {
        let db = state.db.lock().expect("database lock");
        read_session(&db, &headers).is_some()
    };
    if !signed_in {
        return landing_response(&state, query.next.as_deref().unwrap_or(""), notice);
    }
    let db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&db, &headers) else {
        return landing_response(&state, query.next.as_deref().unwrap_or(""), notice);
    };
    let server = match store::discord_server_id(&db, session.guild_id) {
        Ok(server) => server,
        Err(err) => return plain_error(err),
    };
    Redirect::to(&here(&server, "")).into_response()
}

async fn server_home(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "schedule",
        "This week",
        present::home,
    )
}

fn landing_response(state: &AppState, next: &str, notice: Option<String>) -> Response {
    let next = auth::safe_next(next).unwrap_or_default();
    let ready = oauth_ready(state);
    match present::landing(
        &state.invite_url,
        &next,
        notice.as_deref().unwrap_or(""),
        ready,
    ) {
        Ok(body) => html_ok(body, notice.is_some()),
        Err(err) => plain_error(err),
    }
}

fn oauth_ready(state: &AppState) -> bool {
    !state.client_id.trim().is_empty() && !state.client_secret.trim().is_empty()
}

async fn require_login(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if auth::is_public(&path) {
        return next.run(request).await;
    }
    let signed_in = {
        let db = state.db.lock().expect("database lock");
        read_session(&db, request.headers()).is_some()
    };
    if signed_in {
        return next.run(request).await;
    }
    if request.method() == Method::GET || request.method() == Method::HEAD {
        let target = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        return Redirect::to(&auth::login_location(target)).into_response();
    }
    redirect("/", "Sign in with Discord.")
}

async fn auth_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Response {
    if !oauth_ready(&state) {
        return redirect("/", "Sign-in is not configured yet.");
    }
    let Some(redirect_uri) = oauth_redirect(&state, &headers) else {
        return redirect("/", "Set MARIAN_PUBLIC_URL so Discord can send you back.");
    };
    let state_token = match auth::random_token() {
        Ok(token) => token,
        Err(err) => return plain_error(err),
    };
    let next = query
        .next
        .as_deref()
        .and_then(auth::safe_next)
        .unwrap_or_default();
    let secure = cookie_secure(&state, &headers);
    let mut response = Redirect::to(&auth::authorize_url(
        &state.client_id,
        &redirect_uri,
        &state_token,
    ))
    .into_response();
    set_cookie(
        &mut response,
        "marian_oauth",
        &auth::pack_oauth(&state_token, &next),
        600,
        secure,
    );
    response
}

async fn auth_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DiscordCallback>,
) -> Response {
    if query.error.is_some() {
        return redirect("/", "Sign-in was cancelled.");
    }
    let Some(code) = query.code.as_deref().filter(|code| !code.is_empty()) else {
        return redirect("/", "Discord didn't send a sign-in code.");
    };
    let Some(returned) = query.state.as_deref() else {
        return redirect("/", "That sign-in expired. Try again.");
    };
    let Some((expected, next)) =
        cookie(&headers, "marian_oauth").and_then(|raw| auth::unpack_oauth(&raw))
    else {
        return redirect("/", "That sign-in expired. Try again.");
    };
    if returned != expected {
        return redirect("/", "That sign-in expired. Try again.");
    }
    let Some(redirect_uri) = oauth_redirect(&state, &headers) else {
        return redirect("/", "Set MARIAN_PUBLIC_URL so Discord can send you back.");
    };
    let access = match discord_token(&state, code, &redirect_uri).await {
        Ok(token) => token,
        Err(err) => return redirect("/", &err),
    };
    let me = match discord_me(&state, &access).await {
        Ok(me) => me,
        Err(err) => return redirect("/", &err),
    };
    let member_of = match discord_guilds(&state, &access).await {
        Ok(ids) => ids,
        Err(err) => return redirect("/", &err),
    };
    let mut db = state.db.lock().expect("database lock");
    let guilds = match store::list_guilds(&db) {
        Ok(guilds) => guilds,
        Err(err) => return plain_error(err),
    };
    let pairs: Vec<(i64, String)> = guilds
        .iter()
        .filter_map(|guild| Some((guild.id, guild.discord_id.clone()?)))
        .collect();
    let allowed = auth::guilds_in_common(&pairs, &member_of);
    if allowed.is_empty() {
        return redirect("/", "Marian isn't in a server you belong to yet.");
    }
    let guild_id = allowed[0];
    let name = auth::discord_display_name(me.global_name.as_deref(), &me.username);
    let user = match logic::ensure_discord_user(&mut db, guild_id, &me.id, &name) {
        Ok(user) => user,
        Err(err) => return plain_error(err),
    };
    let token = match auth::random_token() {
        Ok(token) => token,
        Err(err) => return plain_error(err),
    };
    if let Err(err) =
        store::insert_web_session(&mut db, &token, &me.id, &name, guild_id, user.id, &allowed)
    {
        return plain_error(err);
    }
    drop(db);
    let dest = if next.is_empty() {
        "/".to_string()
    } else {
        next
    };
    let mut response = Redirect::to(&dest).into_response();
    let secure = cookie_secure(&state, &headers);
    set_cookie(
        &mut response,
        "marian_session",
        &token,
        30 * 24 * 3600,
        secure,
    );
    clear_cookie(&mut response, "marian_oauth");
    clear_cookie(&mut response, "marian_guild");
    clear_cookie(&mut response, "marian_user");
    response
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers, "marian_session") {
        let db = state.db.lock().expect("database lock");
        let _ = store::delete_web_session(&db, &token);
    }
    let mut response = Redirect::to("/").into_response();
    clear_cookie(&mut response, "marian_session");
    clear_cookie(&mut response, "marian_guild");
    clear_cookie(&mut response, "marian_user");
    response
}

fn oauth_redirect(state: &AppState, headers: &HeaderMap) -> Option<String> {
    auth::redirect_uri(
        &state.public_url,
        headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok()),
        headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok()),
    )
}

fn cookie_secure(state: &AppState, headers: &HeaderMap) -> bool {
    auth::cookie_secure(
        &state.public_url,
        headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok()),
    )
}

async fn discord_token(state: &AppState, code: &str, redirect_uri: &str) -> Result<String, String> {
    let response = state
        .http
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("client_id", state.client_id.as_str()),
            ("client_secret", state.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .map_err(|_| "Discord didn't answer the sign-in.".to_string())?;
    if !response.status().is_success() {
        tracing::warn!("discord token exchange returned {}", response.status());
        return Err("Discord didn't accept the sign-in. Try again.".into());
    }
    let body: DiscordToken = response
        .json()
        .await
        .map_err(|_| "Discord sent an unexpected sign-in reply.".to_string())?;
    if body.access_token.is_empty() {
        return Err("Discord sent an unexpected sign-in reply.".into());
    }
    Ok(body.access_token)
}

async fn discord_me(state: &AppState, access: &str) -> Result<DiscordMe, String> {
    let response = state
        .http
        .get("https://discord.com/api/users/@me")
        .bearer_auth(access)
        .send()
        .await
        .map_err(|_| "Discord didn't answer with your account.".to_string())?;
    if !response.status().is_success() {
        tracing::warn!("discord identity returned {}", response.status());
        return Err("Discord didn't share your account. Try again.".into());
    }
    response
        .json()
        .await
        .map_err(|_| "Discord sent an unexpected account.".into())
}

async fn discord_guilds(state: &AppState, access: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut after = String::new();
    for _ in 0..5 {
        let mut endpoint = "https://discord.com/api/users/@me/guilds?limit=200".to_string();
        if !after.is_empty() {
            endpoint.push_str("&after=");
            endpoint.push_str(&auth::encode(&after));
        }
        let response = state
            .http
            .get(endpoint)
            .bearer_auth(access)
            .send()
            .await
            .map_err(|_| "Discord didn't answer with your servers.".to_string())?;
        if !response.status().is_success() {
            tracing::warn!("discord guilds returned {}", response.status());
            return Err("Discord didn't share your servers. Try again.".into());
        }
        let guilds: Vec<DiscordGuild> = response
            .json()
            .await
            .map_err(|_| "Discord sent an unexpected server list.".to_string())?;
        let page = guilds.len();
        if let Some(last) = guilds.last() {
            after = last.id.clone();
        }
        ids.extend(guilds.into_iter().map(|guild| guild.id));
        if page < 200 {
            break;
        }
    }
    Ok(ids)
}

#[derive(serde::Deserialize)]
struct NextQuery {
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct DiscordCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct DiscordToken {
    access_token: String,
}

#[derive(serde::Deserialize)]
struct DiscordMe {
    id: String,
    username: String,
    global_name: Option<String>,
}

#[derive(serde::Deserialize)]
struct DiscordGuild {
    id: String,
}

async fn drafts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(&state, &headers, &server, "drafts", "Drafts", |page| {
        present::list_page(page, "drafts")
    })
}

async fn scheduled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "scheduled",
        "Scheduled posts",
        |page| present::list_page(page, "scheduled"),
    )
}

async fn archive_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(&state, &headers, &server, "archive", "Archive", |page| {
        present::list_page(page, "archive")
    })
}

async fn activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(&state, &headers, &server, "activity", "Activity", |page| {
        present::list_page(page, "activity")
    })
}

async fn recurring(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "recurring",
        "Recurring",
        present::recurring,
    )
}

async fn series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, id)): Path<(String, i64)>,
) -> Response {
    html(&state, &headers, &server, "recurring", "Series", |page| {
        present::series_page(page, id)
    })
}

async fn presets_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "presets",
        "Presets",
        present::presets,
    )
}

async fn channels_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "channels",
        "Channels",
        present::channels,
    )
}

async fn commands_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "commands",
        "Command desk",
        present::commands_page,
    )
}

async fn settings_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "settings",
        "Settings",
        present::settings_page,
    )
}

async fn show_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    html(&state, &headers, &server, "event", "Event", move |page| {
        let event = event_for_guild(page.conn, page.guild_id, &public_id)?;
        present::event_page(page, event.id)
    })
}

async fn new_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    html(&state, &headers, &server, "event", "New event", |page| {
        let preset = query
            .get("preset")
            .and_then(|raw| raw.parse::<i64>().ok())
            .and_then(|id| {
                store::preset(page.conn, id)
                    .ok()
                    .flatten()
                    .filter(|preset| preset.guild_id == page.guild_id)
                    .and_then(|preset| serde_json::from_str::<PresetBody>(&preset.body).ok())
            });
        present::form_page(page, &present::blank_form(&server, preset))
    })
}

async fn edit_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "event",
        "Edit event",
        move |page| {
            let guild = store::guild(page.conn, page.guild_id)?.ok_or(OpError::NotFound)?;
            let tz = clock::zone(&guild.timezone)?;
            let event = event_for_guild(page.conn, page.guild_id, &public_id)?;
            present::form_page(page, &present::form_from_event(page.conn, event.id, tz)?)
        },
    )
}

async fn create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    save_form(&state, &headers, &server, None, map)
}

async fn save_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    save_form(&state, &headers, &server, Some(public_id), map)
}

fn save_form(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    id: Option<String>,
    map: HashMap<String, String>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, server) {
        return guild_error(err);
    }
    let existing = match id.as_deref() {
        Some(public_id) => match store::event_by_public(&db, public_id) {
            Ok(Some(event)) if event.guild_id == session.guild_id => Some(event),
            Ok(_) => return render_missing(state, &db, &session),
            Err(err) => return plain_error(err),
        },
        None => None,
    };
    let guild = match store::guild(&db, session.guild_id) {
        Ok(Some(guild)) => guild,
        Ok(None) => return render_missing(state, &db, &session),
        Err(err) => return plain_error(err),
    };
    let tz = match clock::zone(&guild.timezone) {
        Ok(tz) => tz,
        Err(err) => return plain_error(err),
    };
    let input = match present::input_from_form(
        &map,
        session.guild_id,
        session.user_id,
        tz,
        existing.as_ref(),
    ) {
        Ok(input) => input,
        Err(err) => {
            return form_again(
                state,
                &db,
                &session,
                &map,
                existing.as_ref(),
                tz,
                err.to_string(),
            );
        }
    };
    match logic::save_event(&mut db, input) {
        Ok(event_id) => {
            let saved = store::event(&db, event_id).ok().flatten();
            let public_id = saved
                .as_ref()
                .map(|event| event.public_id.clone())
                .unwrap_or_default();
            let ballot = saved.as_ref().is_some_and(|event| event.status == "ballot");
            let series_id = saved.as_ref().and_then(|event| event.series_id);
            let once = series_id
                .and_then(|id| store::series(&db, id).ok().flatten())
                .is_some_and(|series| series.cadence == "once");
            let round_id = series_id.and_then(|id| logic::open_round_id(&db, id).ok().flatten());
            drop(db);
            if ballot {
                if let Some(round_id) = round_id {
                    enqueue(&state.discord, DiscordJob::poll(round_id));
                }
                let path = if once {
                    format!("/events/{public_id}")
                } else {
                    format!("/recurring/{}", series_id.unwrap_or(0))
                };
                redirect(
                    &here(server, &path),
                    "Voting is open. The signup post replaces the day vote when voting closes.",
                )
            } else {
                enqueue(&state.discord, DiscordJob::sync(event_id));
                redirect(&here(server, &format!("/events/{public_id}")), "Saved.")
            }
        }
        Err(OpError::NotFound) => render_missing(state, &db, &session),
        Err(err) => form_again(
            state,
            &db,
            &session,
            &map,
            existing.as_ref(),
            tz,
            err.to_string(),
        ),
    }
}

fn form_again(
    state: &AppState,
    db: &Connection,
    session: &Session,
    map: &HashMap<String, String>,
    existing: Option<&store::Event>,
    tz: chrono_tz::Tz,
    error: String,
) -> Response {
    let server = store::discord_server_id(db, session.guild_id).unwrap_or_default();
    let draft = present::form_from_map(map, &server, existing, error, tz);
    let page = page_for(state, db, session, "event", "Edit event");
    match present::form_page(&page, &draft) {
        Ok(body) => html_ok(body, session.notice.is_some()),
        Err(err) => plain_error(err),
    }
}

async fn signup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let option_id = map
        .get("option_id")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/events/{public_id}")),
        move |db, session| {
            let event = event_for_guild(db, session.guild_id, &public_id)?;
            let notice = logic::signup(db, event.id, option_id, session.user_id)?;
            Ok((notice, Some(event.id)))
        },
    )
}

async fn response(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let option_id = map
        .get("option_id")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let user_id = map
        .get("user_id")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let action = map.get("action").cloned().unwrap_or_else(|| "add".into());
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/events/{public_id}")),
        move |db, session| {
            let event = event_for_guild(db, session.guild_id, &public_id)?;
            let notice = logic::organizer_response(
                db,
                event.id,
                session.user_id,
                user_id,
                option_id,
                &action,
            )?;
            Ok((notice, Some(event.id)))
        },
    )
}

async fn archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/events/{public_id}")),
        move |db, session| {
            let event = event_for_guild(db, session.guild_id, &public_id)?;
            let notice = logic::archive_event(db, event.id, session.user_id)?;
            Ok((notice, Some(event.id)))
        },
    )
}

async fn delete_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    let gone = {
        let db = state.db.lock().expect("database lock");
        store::event_by_public(&db, &public_id)
            .ok()
            .flatten()
            .map(|event| matches!(event.status.as_str(), "draft" | "scheduled" | "failed"))
    };
    let back = if gone.unwrap_or(false) {
        here(&server, "")
    } else {
        here(&server, &format!("/events/{public_id}"))
    };
    mutate(&state, &headers, &server, &back, move |db, session| {
        let event = event_for_guild(db, session.guild_id, &public_id)?;
        let notice = logic::delete_event(db, event.id, session.user_id)?;
        Ok((notice, Some(event.id)))
    })
}

async fn duplicate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let id = match event_for_guild(&db, session.guild_id, &public_id) {
        Ok(event) => event.id,
        Err(OpError::NotFound) => return render_missing(&state, &db, &session),
        Err(err) => return plain_error(err),
    };
    match logic::duplicate_event(&mut db, id, session.user_id) {
        Ok(new_id) => {
            let copy = store::event(&db, new_id)
                .ok()
                .flatten()
                .map(|event| event.public_id)
                .unwrap_or_default();
            drop(db);
            redirect(
                &here(&server, &format!("/events/{copy}/edit")),
                "A draft copy is ready, one week later.",
            )
        }
        Err(err) => redirect(
            &here(&server, &format!("/events/{public_id}")),
            &err.to_string(),
        ),
    }
}

async fn skip(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/events/{public_id}")),
        move |db, session| {
            let event = event_for_guild(db, session.guild_id, &public_id)?;
            let notice = logic::skip_event(db, event.id, session.user_id)?;
            Ok((notice, None))
        },
    )
}

async fn series_vote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, id)): Path<(String, i64)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let back = vote_back(&db, &server, id);
    let Some(mut session) = read_session(&db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let intent = map.get("intent").map(String::as_str).unwrap_or("cast");
    let result = match intent {
        "settings" => match logic::spec_from_map(&map) {
            Ok(spec) => logic::save_vote_settings(&mut db, id, session.user_id, spec),
            Err(err) => Err(err),
        },
        "week" => logic::save_week(&mut db, id, session.user_id, &map),
        "close" => logic::close_series(&mut db, id, session.user_id),
        "pin" => {
            let Some(choice) = map.get("choice").and_then(|value| value.parse().ok()) else {
                return redirect(&back, "Pick a choice.");
            };
            let poll = map.get("poll").cloned().unwrap_or_default();
            logic::pin_series(&mut db, id, session.user_id, &poll, choice)
        }
        _ => {
            let Some(choice) = map.get("choice").and_then(|value| value.parse().ok()) else {
                return redirect(&back, "Pick a choice.");
            };
            let poll = map.get("poll").cloned().unwrap_or_default();
            logic::cast_series(&mut db, id, session.user_id, &poll, choice)
        }
    };
    match result {
        Ok(effect) => {
            drop(db);
            for job in effect.jobs {
                enqueue(&state.discord, job);
            }
            redirect(&back, &effect.notice)
        }
        Err(err) => redirect(&back, &err.to_string()),
    }
}

async fn end_series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, id)): Path<(String, i64)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/recurring/{id}")),
        |db, session| {
            let notice = logic::end_series(db, id, session.user_id)?;
            Ok((notice, None))
        },
    )
}

async fn save_preset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, "/presets"),
        |db, session| {
            let name = map.get("name").cloned().unwrap_or_default();
            let options = map
                .get("options")
                .map(|raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(|part| library_option(db, session.guild_id, part))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let body = PresetBody {
                multiple_signups: map.contains_key("multiple"),
                options,
            };
            logic::save_preset(db, session.guild_id, session.user_id, &name, &body)?;
            Ok((format!("Saved the {name} preset."), None))
        },
    )
}

async fn delete_preset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, id)): Path<(String, i64)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, "/presets"),
        |db, session| {
            logic::delete_preset(db, id, session.user_id)?;
            Ok(("Removed the preset.".into(), None))
        },
    )
}

async fn save_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, "/presets"),
        |db, session| {
            let name = map.get("name").cloned().unwrap_or_default();
            let emoji = map.get("emoji").cloned().unwrap_or_default();
            logic::library_add(db, session.guild_id, session.user_id, &name, &emoji)?;
            Ok((format!("Added {name} to the option library."), None))
        },
    )
}

fn library_option(conn: &Connection, guild_id: i64, name: &str) -> OptionInput {
    let known = store::list_library(conn, guild_id).ok().and_then(|rows| {
        rows.into_iter()
            .find(|row| row.name.eq_ignore_ascii_case(name))
    });
    OptionInput {
        id: None,
        name: name.to_string(),
        emoji: known.map(|row| row.emoji).unwrap_or_else(|| "•".into()),
        capacity: None,
        waitlist: false,
        role_id: None,
        reminders_enabled: true,
    }
}

async fn create_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, "/channels"),
        |db, session| {
            let name = map.get("name").cloned().unwrap_or_default();
            let kind = map.get("kind").cloned().unwrap_or_else(|| "text".into());
            logic::create_channel(db, session.guild_id, session.user_id, &name, &kind)?;
            Ok((format!("Created #{}.", logic::slug_channel(&name)), None))
        },
    )
}

async fn run_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let line = map.get("line").cloned().unwrap_or_default();
    let reply = commands::execute(&mut db, session.guild_id, session.user_id, &line);
    let ids = events_to_refresh(&db, &line, &reply);
    drop(db);
    for id in ids {
        enqueue(&state.discord, DiscordJob::sync(id));
        enqueue(&state.discord, DiscordJob::attend(id));
    }
    redirect(&here(&server, "/commands"), &clip(&reply, 700))
}

async fn save_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, "/settings");
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let user = match store::user(&db, session.user_id) {
        Ok(Some(user)) => user,
        _ => return redirect(&back, "Sign in again before changing settings."),
    };
    if !user.organizer {
        return redirect(
            &back,
            "Only someone who runs the server can change settings.",
        );
    }
    let mut settings = match store::settings(&db, session.guild_id) {
        Ok(settings) => settings,
        Err(err) => return redirect(&back, &err.to_string()),
    };
    let timezone = map.get("timezone").cloned().unwrap_or_else(|| "UTC".into());
    if clock::zone(&timezone).is_err() {
        return redirect(&back, "That timezone isn't one Marian knows.");
    }
    match apply_settings(&mut settings, &map) {
        Ok(()) => {}
        Err(err) => return redirect(&back, &err.to_string()),
    }
    if let Err(err) = db.execute(
        "UPDATE guilds SET timezone = ? WHERE id = ?",
        rusqlite::params![timezone, session.guild_id],
    ) {
        return redirect(&back, &err.to_string());
    }
    if let Err(err) = store::update_settings(&db, &settings) {
        return redirect(&back, &err.to_string());
    }
    drop(db);
    redirect(&back, "Settings saved.")
}

fn apply_settings(settings: &mut Settings, map: &HashMap<String, String>) -> Result<(), OpError> {
    settings.default_reminder_minutes = bounded(map, "default_reminder_minutes", 1, 10080)?;
    settings.auto_archive_hours = bounded(map, "auto_archive_hours", 1, 168)?;
    settings.post_lead_hours = bounded(map, "post_lead_hours", 0, 336)?;
    settings.digest = match map.get("digest").map(String::as_str).unwrap_or("off") {
        "daily" => "daily".into(),
        "weekly" => "weekly".into(),
        _ => "off".into(),
    };
    settings.role_event = optional_id(map, "role_event");
    settings.role_edit = optional_id(map, "role_edit");
    settings.role_delete = optional_id(map, "role_delete");
    settings.role_channel = optional_id(map, "role_channel");
    settings.role_signup = optional_id(map, "role_signup");
    settings.thread_reminders = map.contains_key("thread_reminders");
    settings.auto_archive = map.contains_key("auto_archive");
    settings.event_threads = map.contains_key("event_threads");
    settings.auto_join_threads = map.contains_key("auto_join_threads");
    settings.show_attendees = map.contains_key("show_attendees");
    settings.hour_24 = map.contains_key("hour_24");
    settings.localize_time = map.contains_key("localize_time");
    settings.purge_channels = map.contains_key("purge_channels");
    settings.temp_roles = map.contains_key("temp_roles");
    settings.calendar_auto_sync = map.contains_key("calendar_auto_sync");
    settings.park_clock = map.contains_key("park_clock");
    Ok(())
}

fn bounded(map: &HashMap<String, String>, key: &str, min: i64, max: i64) -> Result<i64, OpError> {
    let raw = map.get(key).map(String::as_str).unwrap_or("");
    let value = raw
        .parse::<i64>()
        .map_err(|_| OpError::message(format!("{key} needs to be a number.")))?;
    if !(min..=max).contains(&value) {
        return Err(OpError::message(format!(
            "{key} needs to be between {min} and {max}."
        )));
    }
    Ok(value)
}

fn optional_id(map: &HashMap<String, String>, key: &str) -> Option<i64> {
    map.get(key)
        .and_then(|value| value.parse().ok())
        .filter(|id| *id > 0)
}

async fn toggle_reminders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let enabled = map.get("enabled").map(|value| value == "1").unwrap_or(true);
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, ""),
        |db, session| {
            let notice = logic::set_user_reminders(db, session.user_id, enabled)?;
            Ok((notice, None))
        },
    )
}

async fn event_calendar(
    State(state): State<AppState>,
    Path((server, file)): Path<(String, String)>,
) -> Response {
    let db = state.db.lock().expect("database lock");
    let Some(guild) = store::guild_by_discord(&db, &server).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No server").into_response();
    };
    let Some(event) = calendar_event(&db, &file).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    };
    if event.guild_id != guild.id {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    }
    match ics::one_event(&db, event.id) {
        Ok(body) => calendar_response(body),
        Err(OpError::NotFound) => (StatusCode::NOT_FOUND, "No event").into_response(),
        Err(err) => plain_error(err),
    }
}

async fn legacy_event_calendar(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Response {
    let db = state.db.lock().expect("database lock");
    if let Some(token) = personal_token(&file) {
        let Some(discord_id) = store::calendar_discord_id(&db, token).ok().flatten() else {
            return (StatusCode::NOT_FOUND, "No calendar").into_response();
        };
        return match ics::user_feed(&db, &discord_id) {
            Ok(body) => calendar_response(body),
            Err(err) => plain_error(err),
        };
    }
    let Some(event) = calendar_event(&db, &file).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    };
    match ics::one_event(&db, event.id) {
        Ok(body) => calendar_response(body),
        Err(OpError::NotFound) => (StatusCode::NOT_FOUND, "No event").into_response(),
        Err(err) => plain_error(err),
    }
}

fn personal_token(file: &str) -> Option<&str> {
    let token = file.strip_suffix(".ics")?;
    if token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(token)
    } else {
        None
    }
}

fn calendar_response(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "text/calendar; charset=utf-8")],
        body,
    )
        .into_response()
}

fn event_for_guild(
    conn: &Connection,
    guild_id: i64,
    public_id: &str,
) -> Result<store::Event, OpError> {
    let event = store::event_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if event.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    Ok(event)
}

fn calendar_event(conn: &Connection, file: &str) -> Result<Option<store::Event>, OpError> {
    let token = file.trim_end_matches(".ics");
    if let Some(public_id) = crate::ids::parse_event_id(token) {
        if let Some(event) = store::event_by_public(conn, public_id)? {
            return Ok(Some(event));
        }
    }
    if let Ok(id) = token.parse::<i64>() {
        return store::event(conn, id);
    }
    Ok(None)
}

fn vote_back(conn: &Connection, server: &str, series_id: i64) -> String {
    let once = store::series(conn, series_id)
        .ok()
        .flatten()
        .is_some_and(|series| series.cadence == "once");
    if once {
        if let Ok(events) = store::events_in_series(conn, series_id) {
            let event = events
                .iter()
                .find(|event| event.status == "ballot")
                .or_else(|| events.first());
            if let Some(event) = event {
                return here(server, &format!("/events/{}", event.public_id));
            }
        }
    }
    here(server, &format!("/recurring/{series_id}"))
}

fn here(server: &str, rest: &str) -> String {
    format!("/servers/{server}{rest}")
}

fn bind_server(conn: &mut Connection, session: &mut Session, server: &str) -> Result<(), OpError> {
    let guild = store::guild_by_discord(conn, server)?.ok_or(OpError::NotFound)?;
    bind_guild(conn, session, guild.id)
}

fn bind_guild(conn: &mut Connection, session: &mut Session, guild_id: i64) -> Result<(), OpError> {
    if guild_id <= 0 || !session.allowed.contains(&guild_id) {
        return Err(OpError::message("That server isn't one you signed into."));
    }
    if store::guild(conn, guild_id)?.is_none() {
        return Err(OpError::NotFound);
    }
    let user = logic::ensure_discord_user(
        conn,
        guild_id,
        &session.discord_user_id,
        &session.display_name,
    )?;
    session.guild_id = guild_id;
    session.user_id = user.id;
    Ok(())
}

fn guild_error(err: OpError) -> Response {
    match err {
        OpError::NotFound => (StatusCode::NOT_FOUND, "That server isn't here.").into_response(),
        other => redirect("/", &other.to_string()),
    }
}

fn legacy_redirect(state: &AppState, headers: &HeaderMap, rest: &str) -> Response {
    let db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    let server = match store::discord_server_id(&db, session.guild_id) {
        Ok(server) => server,
        Err(err) => return plain_error(err),
    };
    Redirect::to(&here(&server, rest)).into_response()
}

async fn legacy_drafts(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/drafts")
}

async fn legacy_scheduled(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/scheduled")
}

async fn legacy_recurring(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/recurring")
}

async fn legacy_archive(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/archive")
}

async fn legacy_presets(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/presets")
}

async fn legacy_channels(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/channels")
}

async fn legacy_commands(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/commands")
}

async fn legacy_activity(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/activity")
}

async fn legacy_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/settings")
}

async fn legacy_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    legacy_event_path(&state, &headers, &id, "")
}

async fn legacy_edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    legacy_event_path(&state, &headers, &id, "/edit")
}

fn legacy_event_path(state: &AppState, headers: &HeaderMap, id: &str, suffix: &str) -> Response {
    let db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    let event = match calendar_event(&db, id) {
        Ok(Some(event)) => event,
        Ok(None) => return (StatusCode::NOT_FOUND, "No event").into_response(),
        Err(err) => return plain_error(err),
    };
    if !session.allowed.contains(&event.guild_id) {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    }
    let server = match store::discord_server_id(&db, event.guild_id) {
        Ok(server) => server,
        Err(err) => return plain_error(err),
    };
    Redirect::to(&here(
        &server,
        &format!("/events/{}{suffix}", event.public_id),
    ))
    .into_response()
}

fn mutate(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    back: &str,
    body: impl FnOnce(&mut Connection, &Session) -> Result<(String, Option<i64>), OpError>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, server) {
        return guild_error(err);
    }
    match body(&mut db, &session) {
        Ok((notice, sync)) => {
            drop(db);
            if let Some(event_id) = sync {
                enqueue(&state.discord, DiscordJob::sync(event_id));
                enqueue(&state.discord, DiscordJob::attend(event_id));
            }
            redirect(back, &notice)
        }
        Err(err) => redirect(back, &err.to_string()),
    }
}

fn html(
    state: &AppState,
    headers: &HeaderMap,
    server: &str,
    nav: &str,
    title: &str,
    render: impl FnOnce(&Page<'_>) -> Result<String, OpError>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, server) {
        return guild_error(err);
    }
    let page = page_for(state, &db, &session, nav, title);
    match render(&page) {
        Ok(body) => html_ok(body, session.notice.is_some()),
        Err(OpError::NotFound) => match present::not_found(&page) {
            Ok(body) => {
                let mut response = Html(body).into_response();
                *response.status_mut() = StatusCode::NOT_FOUND;
                if session.notice.is_some() {
                    clear_cookie(&mut response, "marian_notice");
                }
                response
            }
            Err(err) => plain_error(err),
        },
        Err(err) => plain_error(err),
    }
}

fn render_missing(state: &AppState, db: &Connection, session: &Session) -> Response {
    let page = page_for(state, db, session, "event", "Missing");
    match present::not_found(&page) {
        Ok(body) => {
            let mut response = Html(body).into_response();
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
        Err(err) => plain_error(err),
    }
}

fn page_for<'a>(
    state: &'a AppState,
    db: &'a Connection,
    session: &'a Session,
    nav: &'a str,
    title: &'a str,
) -> Page<'a> {
    Page {
        conn: db,
        guild_id: session.guild_id,
        user_id: session.user_id,
        notice: session.notice.as_deref(),
        invite_url: &state.invite_url,
        nav,
        title,
        allowed_guilds: &session.allowed,
    }
}

fn html_ok(body: String, clear_notice: bool) -> Response {
    let mut response = Html(body).into_response();
    if clear_notice {
        clear_cookie(&mut response, "marian_notice");
    }
    response
}

fn plain_error(err: OpError) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
}

fn redirect(path: &str, notice: &str) -> Response {
    let path = if path.starts_with('/') && !path.starts_with("//") {
        path
    } else {
        "/"
    };
    let mut response = Redirect::to(path).into_response();
    let value = format!(
        "marian_notice={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=120",
        percent_encode(&clip(notice, 700))
    );
    if let Ok(header) = HeaderValue::from_str(&value) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
    response
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: u64, secure: bool) {
    let secure = if secure { "; Secure" } else { "" };
    let raw = format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    if let Ok(header) = HeaderValue::from_str(&raw) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

fn clear_cookie(response: &mut Response, name: &str) {
    let raw = format!("{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if let Ok(header) = HeaderValue::from_str(&raw) {
        response.headers_mut().append(header::SET_COOKIE, header);
    }
}

fn read_session(conn: &Connection, headers: &HeaderMap) -> Option<Session> {
    let token = cookie(headers, "marian_session")?;
    let row = store::web_session(conn, &token).ok()??;
    let fresh = clock::parse_stamp(&row.created_at)
        .map(|at| Utc::now() - at < chrono::Duration::days(30))
        .unwrap_or(false);
    if !fresh {
        let _ = store::delete_web_session(conn, &token);
        return None;
    }
    let user = store::user(conn, row.user_id).ok()??;
    if user.guild_id != row.guild_id
        || user.discord_id.as_deref() != Some(row.discord_user_id.as_str())
    {
        let _ = store::delete_web_session(conn, &token);
        return None;
    }
    let notice = cookie(headers, "marian_notice").map(|value| percent_decode(&value));
    Some(Session {
        discord_user_id: row.discord_user_id,
        display_name: row.display_name,
        guild_id: row.guild_id,
        user_id: row.user_id,
        allowed: row.guilds,
        notice,
    })
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let mut bits = part.trim().splitn(2, '=');
        if bits.next() == Some(name) {
            return bits.next().map(|value| value.to_string());
        }
    }
    None
}

fn enqueue(sender: &UnboundedSender<DiscordJob>, job: DiscordJob) {
    if sender.send(job).is_err() {
        tracing::warn!("Discord job queue is closed");
    }
}

pub fn mentioned_events(line: &str, reply: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    collect_ids(line, &mut ids);
    collect_ids(reply, &mut ids);
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub fn events_to_refresh(conn: &Connection, line: &str, reply: &str) -> Vec<i64> {
    let mut ids = mentioned_events(line, reply);
    for public_id in public_ids_in(line).into_iter().chain(public_ids_in(reply)) {
        if let Ok(Some(event)) = store::event_by_public(conn, &public_id) {
            ids.push(event.id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn public_ids_in(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for marker in ["/events/", "event:"] {
        let mut rest = text;
        while let Some(pos) = rest.find(marker) {
            rest = &rest[pos + marker.len()..];
            if let Some(id) = crate::ids::parse_event_id(rest) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn collect_ids(text: &str, ids: &mut Vec<i64>) {
    let markers = ["/events/", "event:", "Updated event "];
    for marker in markers {
        let mut rest = text;
        while let Some(pos) = rest.find(marker) {
            rest = &rest[pos + marker.len()..];
            if crate::ids::parse_event_id(rest).is_some() {
                continue;
            }
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(id) = digits.parse::<i64>() {
                if id > 0 {
                    ids.push(id);
                }
            }
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn percent_encode(text: &str) -> String {
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

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn invite_url(client_id: &str) -> String {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return String::new();
    }
    format!(
        "https://discord.com/api/oauth2/authorize?client_id={client_id}&permissions=326686174272&scope=bot%20applications.commands"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_round_trip_and_event_ids() {
        let encoded = percent_encode("Posted Shore supper.");
        assert_eq!(percent_decode(&encoded), "Posted Shore supper.");
        let ids = mentioned_events("event:12", "Open /events/12. Updated event 4.");
        assert_eq!(ids, vec![4, 12]);
    }

    #[tokio::test]
    async fn sign_in_is_required() {
        let mut conn = store::open_memory().unwrap();
        conn.execute(
            "INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99')",
            [],
        )
        .unwrap();
        store::insert_default_settings(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO users (guild_id, name, handle, color, reminders_enabled, organizer, discord_id)
             VALUES (1, 'Nell', 'nell', '#1e3a6e', 1, 1, '42')",
            [],
        )
        .unwrap();
        let user_id = conn.last_insert_rowid();
        store::insert_web_session(
            &mut conn,
            "session-token-value",
            "42",
            "Nell",
            1,
            user_id,
            &[1],
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            discord: tx,
            invite_url: "https://discord.com/oauth2/authorize?example".into(),
            http: reqwest::Client::new(),
            client_id: "123".into(),
            client_secret: "test-secret".into(),
            public_url: "http://127.0.0.1:8080".into(),
        };
        let app = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let home = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(home.status(), StatusCode::OK);
        let body = home.text().await.unwrap();
        assert!(body.contains("Sign in with Discord"));
        assert!(body.contains("Events for your server"));
        assert!(body.contains("/static/marian.png"));

        let picture = client
            .get(format!("http://{addr}/static/marian.png"))
            .send()
            .await
            .unwrap();
        assert_eq!(picture.status(), StatusCode::OK);
        assert_eq!(
            picture.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let blocked = client
            .get(format!("http://{addr}/settings"))
            .send()
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            blocked.headers().get(header::LOCATION).unwrap(),
            "/?next=%2Fsettings"
        );

        let open = client
            .get(format!("http://{addr}/servers/99/settings"))
            .header(header::COOKIE, "marian_session=session-token-value")
            .send()
            .await
            .unwrap();
        assert_eq!(open.status(), StatusCode::OK);
        let body = open.text().await.unwrap();
        assert!(body.contains("Signed in as <strong>Nell</strong>"));
        assert!(body.contains("class=\"account\""));
        assert!(body.contains("Calendar feed"));
        assert!(body.contains("class=\"calendar\""));
        assert!(body.contains(".ics"));
        assert!(body.contains("Sign out"));
        assert!(body.contains("class=\"invite\""));
        assert!(body.contains("Invite Marian to Discord"));
        assert!(body.contains("/static/marian.png"));
        assert!(body.contains("servers&#x2f;99/settings"));
        assert!(!body.contains("Acting as"));

        let legacy = client
            .get(format!("http://{addr}/settings"))
            .header(header::COOKIE, "marian_session=session-token-value")
            .send()
            .await
            .unwrap();
        assert_eq!(legacy.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            legacy.headers().get(header::LOCATION).unwrap(),
            "/servers/99/settings"
        );

        let week = client
            .get(format!("http://{addr}/"))
            .header(header::COOKIE, "marian_session=session-token-value")
            .send()
            .await
            .unwrap();
        assert_eq!(week.status(), StatusCode::SEE_OTHER);
        assert_eq!(week.headers().get(header::LOCATION).unwrap(), "/servers/99");

        let start = client
            .get(format!("http://{addr}/auth/discord?next=/events/3"))
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::SEE_OTHER);
        let location = start
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(location.starts_with("https://discord.com/api/oauth2/authorize?"));
        assert!(location.contains("scope=identify%20guilds"));
        assert!(location.contains("client_id=123"));
        assert!(!location.contains("test-secret"));
    }
}
