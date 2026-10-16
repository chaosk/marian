use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Form, MatchedPath, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use diesel::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::assets;
use crate::auth;
use crate::clock;
use crate::commands;
use crate::error::OpError;
use crate::ics;
use crate::jobs::DiscordJob;
use crate::logic::{self, OptionInput, PresetBody};
use crate::present::{self, Page};
use crate::schema;
use crate::store::{self, Connection, Settings};

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    discord: UnboundedSender<DiscordJob>,
    invite_url: String,
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    public_url: String,
    google_client_id: String,
    google_client_secret: String,
    bot_token: String,
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
    let _telemetry = crate::observe::init();

    let database_url = store::database_url();
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
    let google_client_id = env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    let google_client_secret = env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default();
    let status = env::var("MARIAN_STATUS").unwrap_or_default();
    let invite_url = invite_url(&client_id);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut conn = store::open(&database_url)?;
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
        google_client_id,
        google_client_secret,
        bot_token: token.clone(),
    };
    let app = router(state.clone());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        crate::gateway::start(token, state.db.clone(), rx, tx, public_url, status);
        let db = state.db.clone();
        let discord = state.discord.clone();
        let http = state.http.clone();
        let google_client_id = state.google_client_id.clone();
        let google_client_secret = state.google_client_secret.clone();
        let calendar_base = state.public_url.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let span = tracing::info_span!("tick");
                let _enter = span.enter();
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
                sync_google(
                    &db,
                    &http,
                    &google_client_id,
                    &google_client_secret,
                    &calendar_base,
                )
                .await;
            }
        });

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Marian is listening on {addr}");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        Ok(())
    })
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/help", get(show_help))
        .route("/favicon.svg", get(asset))
        .route("/static/{*file}", get(asset))
        .route("/", get(schedule))
        .route("/auth/discord", get(auth_start))
        .route("/auth/discord/callback", get(auth_callback))
        .route("/auth/google", get(google_start))
        .route("/auth/google/start/{state}", get(google_open))
        .route("/auth/google/callback", get(google_callback))
        .route("/auth/google/disconnect", post(google_disconnect))
        .route("/logout", post(logout))
        .route("/servers/{guild_id}", get(server_home))
        .route(
            "/servers/{guild_id}/events/new",
            get(new_event).post(create_event),
        )
        .route("/servers/{guild_id}/events/{id}", get(show_event))
        .route(
            "/servers/{guild_id}/events/{id}/edit",
            get(edit_event).post(save_event),
        )
        .route("/servers/{guild_id}/events/{id}/signup", post(signup))
        .route("/servers/{guild_id}/events/{id}/response", post(response))
        .route("/servers/{guild_id}/events/{id}/archive", post(archive))
        .route("/servers/{guild_id}/events/{id}/delete", post(delete_event))
        .route("/servers/{guild_id}/events/{id}/duplicate", post(duplicate))
        .route("/servers/{guild_id}/events/{id}/skip", post(skip))
        .route("/servers/{guild_id}/drafts", get(drafts))
        .route("/servers/{guild_id}/scheduled", get(scheduled))
        .route("/servers/{guild_id}/recurring", get(recurring))
        .route(
            "/servers/{guild_id}/recurring/{id}",
            get(series).post(save_series),
        )
        .route("/servers/{guild_id}/recurring/{id}/end", post(end_series))
        .route(
            "/servers/{guild_id}/routines",
            get(routines_page).post(create_routine),
        )
        .route("/servers/{guild_id}/routines/new", get(new_routine))
        .route(
            "/servers/{guild_id}/routines/{id}",
            get(show_routine).post(update_routine),
        )
        .route(
            "/servers/{guild_id}/routines/{id}/days",
            post(save_routine_days),
        )
        .route("/servers/{guild_id}/routines/{id}/vote", post(vote_routine))
        .route("/servers/{guild_id}/routines/{id}/stop", post(stop_routine))
        .route("/servers/{guild_id}/archive", get(archive_list))
        .route(
            "/servers/{guild_id}/presets",
            get(presets_page).post(save_preset),
        )
        .route("/servers/{guild_id}/presets/library", post(save_library))
        .route("/servers/{guild_id}/presets/{id}/delete", post(delete_preset))
        .route(
            "/servers/{guild_id}/channels",
            get(channels_page).post(create_channel),
        )
        .route(
            "/servers/{guild_id}/commands",
            get(commands_page).post(run_command),
        )
        .route("/servers/{guild_id}/activity", get(activity))
        .route(
            "/servers/{guild_id}/settings",
            get(settings_page).post(save_settings),
        )
        .route(
            "/servers/{guild_id}/google",
            get(google_page).post(save_google),
        )
        .route("/servers/{guild_id}/me/reminders", post(toggle_reminders))
        .route("/servers/{guild_id}/calendar/{file}", get(event_calendar))
        .route("/drafts", get(legacy_drafts))
        .route("/scheduled", get(legacy_scheduled))
        .route("/recurring", get(legacy_recurring))
        .route("/routines", get(legacy_routines))
        .route("/archive", get(legacy_archive))
        .route("/presets", get(legacy_presets))
        .route("/channels", get(legacy_channels))
        .route("/commands", get(legacy_commands))
        .route("/activity", get(legacy_activity))
        .route("/settings", get(legacy_settings))
        .route("/google", get(legacy_google))
        .route("/events/{id}", get(legacy_event))
        .route("/events/{id}/edit", get(legacy_edit))
        .route("/calendar/{file}", get(legacy_event_calendar))
        .layer(middleware::from_fn_with_state(state.clone(), require_login))
        .layer(http_trace())
        .with_state(state)
}

#[derive(Clone, Copy)]
struct HttpSpan;

impl<B> tower_http::trace::MakeSpan<B> for HttpSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> tracing::Span {
        if quiet_path(request.uri().path()) {
            return tracing::Span::none();
        }
        let route = request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str)
            .unwrap_or("unmatched");
        let name = format!("{} {route}", request.method());
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0);
        let user_agent = user_agent(request.headers());
        let client_ip = client_ip(request.headers(), peer);
        let span = tracing::info_span!(
            "http",
            otel.name = name.as_str(),
            otel.kind = "server",
            "http.method" = %request.method(),
            "http.route" = route,
            "http.status_code" = tracing::field::Empty,
            "http.user_agent" = tracing::field::Empty,
            "http.client_ip" = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
        );
        if let Some(value) = user_agent.as_deref() {
            span.record("http.user_agent", value);
        }
        if let Some(value) = client_ip.as_deref() {
            span.record("http.client_ip", value);
        }
        span
    }
}

#[derive(Clone, Copy)]
struct HttpStatus;

impl<B> tower_http::trace::OnResponse<B> for HttpStatus {
    fn on_response(
        self,
        response: &axum::response::Response<B>,
        latency: std::time::Duration,
        span: &tracing::Span,
    ) {
        span.record("http.status_code", response.status().as_u16());
        span.record("duration_ms", latency.as_secs_f64() * 1000.0);
    }
}

fn http_trace() -> tower_http::trace::TraceLayer<
    tower_http::trace::HttpMakeClassifier,
    HttpSpan,
    (),
    HttpStatus,
    tower_http::trace::DefaultOnBodyChunk,
    tower_http::trace::DefaultOnEos,
    (),
> {
    tower_http::trace::TraceLayer::new_for_http()
        .make_span_with(HttpSpan)
        .on_request(())
        .on_response(HttpStatus)
        .on_failure(())
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::USER_AGENT)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(256).collect())
}

/// The address the proxy accepted, then the original client in
/// `X-Forwarded-For`, then the socket peer.
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
    header_ip(headers, "x-real-ip")
        .or_else(|| forwarded_for(headers))
        .or_else(|| peer.map(|addr| addr.ip().to_canonical().to_string()))
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<String> {
    parse_ip(headers.get(name)?.to_str().ok()?.trim())
}

fn forwarded_for(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-forwarded-for")?.to_str().ok()?;
    value.split(',').find_map(|part| parse_ip(part.trim()))
}

fn parse_ip(text: &str) -> Option<String> {
    let text = text.trim().trim_matches('"');
    let ip = if let Some(inside) = text
        .strip_prefix('[')
        .and_then(|rest| rest.split(']').next())
    {
        inside.parse().ok()?
    } else if let Ok(ip) = text.parse::<std::net::IpAddr>() {
        ip
    } else if let Ok(addr) = text.parse::<SocketAddr>() {
        addr.ip()
    } else {
        return None;
    };
    Some(ip.to_canonical().to_string())
}

fn quiet_path(path: &str) -> bool {
    path == "/healthz"
        || path == "/readyz"
        || path == "/favicon.svg"
        || path.starts_with("/static/")
}

async fn health() -> &'static str {
    "ok"
}

async fn show_help() -> Response {
    match present::help_page() {
        Ok(body) => Html(body).into_response(),
        Err(err) => plain_error(err),
    }
}

async fn ready(State(state): State<AppState>) -> Response {
    let ok = state.db.lock().map(|mut conn| {
        diesel::select(diesel::dsl::sql::<diesel::sql_types::Integer>("1"))
            .get_result::<i32>(&mut *conn)
            .is_ok()
    });
    if ok.unwrap_or(false) {
        (StatusCode::OK, "ok").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
    }
}

async fn asset(request: Request) -> Response {
    assets::response(
        request.uri().path(),
        request.headers().get(header::IF_NONE_MATCH),
    )
}

async fn schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Response {
    let notice = cookie(&headers, "marian_notice").map(|value| percent_decode(&value));
    let signed_in = {
        let mut db = state.db.lock().expect("database lock");
        read_session(&mut db, &headers).is_some()
    };
    if !signed_in {
        return landing_response(&state, query.next.as_deref().unwrap_or(""), notice);
    }
    let mut db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&mut db, &headers) else {
        return landing_response(&state, query.next.as_deref().unwrap_or(""), notice);
    };
    let server = match store::discord_server_id(&mut db, session.guild_id) {
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
    let headers = request.headers().clone();
    let signed_in = {
        let mut db = state.db.lock().expect("database lock");
        read_session(&mut db, &headers).is_some()
    };
    if signed_in {
        refresh_member_roles(&state, &path, &headers).await;
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

struct RoleTarget {
    guild_id: i64,
    user_id: i64,
    guild_discord: String,
    user_discord: String,
}

enum FreshRoles {
    Known(Vec<String>),
    Unavailable,
}

async fn refresh_member_roles(state: &AppState, path: &str, headers: &HeaderMap) {
    if state.bot_token.trim().is_empty() {
        return;
    }
    let Some(target) = load_role_target(state, path, headers) else {
        return;
    };
    let Some((status, body)) =
        discord_member(&state.http, &state.bot_token, &target.guild_discord, &target.user_discord)
            .await
    else {
        tracing::warn!("could not read Discord roles for this member");
        return;
    };
    let FreshRoles::Known(roles) = fresh_roles(status, &body) else {
        tracing::warn!("Discord role lookup returned {status}");
        return;
    };
    store_member_roles(state, &target, &roles);
}

fn load_role_target(state: &AppState, path: &str, headers: &HeaderMap) -> Option<RoleTarget> {
    let server = server_in_path(path)?;
    let mut db = state.db.lock().expect("database lock");
    role_target(&mut db, headers, server)
}

fn store_member_roles(state: &AppState, target: &RoleTarget, roles: &[String]) {
    let mut db = state.db.lock().expect("database lock");
    if let Err(err) = store::replace_discord_roles(&mut db, target.guild_id, target.user_id, roles)
    {
        tracing::warn!("could not store Discord roles: {err}");
    }
}

fn server_in_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/servers/")?;
    let id = rest.split('/').next()?;
    if snowflake(id) { Some(id) } else { None }
}

fn snowflake(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit())
}

fn role_target(conn: &mut Connection, headers: &HeaderMap, server: &str) -> Option<RoleTarget> {
    let session = read_session(conn, headers)?;
    let guild = store::guild_by_discord(conn, server).ok()??;
    if !session.allowed.contains(&guild.id) {
        return None;
    }
    let user = logic::ensure_discord_user(
        conn,
        guild.id,
        &session.discord_user_id,
        &session.display_name,
    )
    .ok()?;
    Some(RoleTarget {
        guild_id: guild.id,
        user_id: user.id,
        guild_discord: server.to_string(),
        user_discord: session.discord_user_id,
    })
}

fn fresh_roles(status: StatusCode, body: &str) -> FreshRoles {
    if status == StatusCode::NOT_FOUND {
        return FreshRoles::Known(Vec::new());
    }
    if !status.is_success() {
        return FreshRoles::Unavailable;
    }
    #[derive(serde::Deserialize)]
    struct Member {
        roles: Vec<String>,
    }
    match serde_json::from_str::<Member>(body) {
        Ok(member) => {
            FreshRoles::Known(member.roles.into_iter().filter(|id| snowflake(id)).collect())
        }
        Err(_) => FreshRoles::Unavailable,
    }
}

async fn discord_member(
    http: &reqwest::Client,
    bot_token: &str,
    guild_discord: &str,
    user_discord: &str,
) -> Option<(StatusCode, String)> {
    if !snowflake(guild_discord) || !snowflake(user_discord) {
        return None;
    }
    let response = crate::observe::send_traced(
        http.get(format!(
            "https://discord.com/api/guilds/{guild_discord}/members/{user_discord}"
        ))
        .header(
            header::AUTHORIZATION,
            format!("Bot {}", bot_token.trim()),
        ),
    )
    .await
    .ok()?;
    let status = response.status();
    let body = response.text().await.ok()?;
    Some((status, body))
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
    let guilds = match store::list_guilds(&mut db) {
        Ok(guilds) => guilds,
        Err(err) => return plain_error(err),
    };
    let pairs: Vec<(i64, String)> = guilds
        .iter()
        .filter_map(|guild| Some((guild.id, guild.discord_id.clone()?)))
        .collect();
    let discord_ids: Vec<String> = member_of.iter().map(|guild| guild.id.clone()).collect();
    let allowed = auth::guilds_in_common(&pairs, &discord_ids);
    if allowed.is_empty() {
        return redirect("/", "Marian isn't in a server you belong to yet.");
    }
    let name = auth::discord_display_name(me.global_name.as_deref(), &me.username);
    let mut signed_in = None;
    for &guild_id in &allowed {
        let discord_id = pairs
            .iter()
            .find(|(id, _)| *id == guild_id)
            .map(|(_, discord_id)| discord_id.as_str())
            .unwrap_or("");
        let user = match logic::ensure_discord_user(&mut db, guild_id, &me.id, &name) {
            Ok(user) => user,
            Err(err) => return plain_error(err),
        };
        let manages = member_of
            .iter()
            .find(|guild| guild.id == discord_id)
            .is_some_and(|guild| guild.manages);
        if let Err(err) = logic::set_organizer(&mut db, user.id, manages) {
            return plain_error(err);
        }
        if signed_in.is_none() {
            signed_in = Some((guild_id, user.id));
        }
    }
    let Some((guild_id, user_id)) = signed_in else {
        return redirect("/", "Marian isn't in a server you belong to yet.");
    };
    let token = match auth::random_token() {
        Ok(token) => token,
        Err(err) => return plain_error(err),
    };
    if let Err(err) =
        store::insert_web_session(&mut db, &token, &me.id, &name, guild_id, user_id, &allowed)
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

async fn sync_google(
    db: &Arc<Mutex<Connection>>,
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    public_base: &str,
) {
    if !crate::google::configured(client_id, client_secret) {
        return;
    }
    let jobs = {
        let mut guard = db.lock().expect("database lock");
        crate::google::plan(&mut guard, public_base)
    };
    let jobs = match jobs {
        Ok(jobs) => jobs,
        Err(err) => {
            tracing::warn!("google calendar: {err}");
            return;
        }
    };
    for job in jobs {
        let outcome = crate::google::push(http, client_id, client_secret, job).await;
        if let crate::google::Outcome::Failed {
            event_id, message, ..
        } = &outcome
        {
            tracing::warn!("google calendar event {event_id}: {message}");
        }
        let mut guard = db.lock().expect("database lock");
        if let Err(err) = crate::google::record(&mut guard, outcome) {
            tracing::warn!("google calendar: {err}");
        }
    }
    let revokes = {
        let mut guard = db.lock().expect("database lock");
        match crate::google::ready_revokes(&mut guard) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("google calendar: {err}");
                Vec::new()
            }
        }
    };
    for access in revokes {
        let discord_user_id = access.discord_user_id.clone();
        if crate::google::forget_remote(http, client_id, client_secret, access).await {
            let mut guard = db.lock().expect("database lock");
            if let Err(err) = crate::google::delete_account(&mut guard, &discord_user_id) {
                tracing::warn!("google calendar: {err}");
            }
        } else {
            tracing::warn!("google calendar disconnect for {discord_user_id} will retry");
        }
    }
}

async fn google_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NextQuery>,
) -> Response {
    let next = query
        .next
        .as_deref()
        .and_then(auth::safe_next)
        .unwrap_or_else(|| "/".into());
    if !crate::google::configured(&state.google_client_id, &state.google_client_secret) {
        return redirect(&next, "Google Calendar is not configured yet.");
    }
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return redirect(&next, "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let session = {
        let mut db = state.db.lock().expect("database lock");
        read_session(&mut db, &headers)
    };
    let Some(session) = session else {
        return redirect(&next, "Sign in with Discord.");
    };
    let state_token = {
        let mut db = state.db.lock().expect("database lock");
        match crate::google::begin_link(&mut db, &session.discord_user_id) {
            Ok(token) => token,
            Err(err) => return plain_error(err),
        }
    };
    let secure = cookie_secure(&state, &headers);
    let mut response = Redirect::to(&crate::google::authorize_url(
        &state.google_client_id,
        &redirect_uri,
        &state_token,
    ))
    .into_response();
    set_cookie(
        &mut response,
        "marian_google",
        &auth::pack_oauth(&state_token, &next),
        600,
        secure,
    );
    response
}

async fn google_open(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    if token.len() != 64 || !token.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return redirect(
            "/",
            "That Google Calendar connection expired. Run /google authorize again.",
        );
    }
    if !crate::google::configured(&state.google_client_id, &state.google_client_secret) {
        return redirect("/", "Google Calendar is not configured yet.");
    }
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return redirect("/", "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let pending = {
        let mut db = state.db.lock().expect("database lock");
        match crate::google::claim_oauth(&mut db, &token) {
            Ok(row) => row.is_some(),
            Err(err) => return plain_error(err),
        }
    };
    if !pending {
        return redirect(
            "/",
            "That Google Calendar connection expired. Run /google authorize again.",
        );
    }
    let secure = cookie_secure(&state, &headers);
    let mut response = Redirect::to(&crate::google::authorize_url(
        &state.google_client_id,
        &redirect_uri,
        &token,
    ))
    .into_response();
    set_cookie(
        &mut response,
        "marian_google",
        &auth::pack_oauth(&token, "/"),
        600,
        secure,
    );
    response
}

fn matched_google_next(headers: &HeaderMap, returned: &str) -> Option<String> {
    let (expected, next) =
        cookie(headers, "marian_google").and_then(|raw| auth::unpack_oauth(&raw))?;
    if expected != returned {
        None
    } else {
        Some(next)
    }
}

async fn google_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<GoogleCallback>,
) -> Response {
    let returned = query.state.clone().unwrap_or_default();
    let Some(next) = matched_google_next(&headers, &returned) else {
        return redirect(
            "/",
            "That Google Calendar connection expired. Run /google authorize again.",
        );
    };
    let dest = if next.is_empty() {
        "/".to_string()
    } else {
        next
    };
    if query.error.is_some() {
        let mut db = state.db.lock().expect("database lock");
        let _ = crate::google::finish_oauth(&mut db, &returned);
        let mut response = redirect(
            &dest,
            "The Google Calendar connection stopped before it finished.",
        );
        clear_cookie(&mut response, "marian_google");
        return response;
    }
    let Some(code) = query.code.as_deref().filter(|code| !code.is_empty()) else {
        return redirect(&dest, "Google didn't send a connection code.");
    };
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return redirect(&dest, "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let discord_user_id = {
        let mut db = state.db.lock().expect("database lock");
        match crate::google::claim_oauth(&mut db, &returned) {
            Ok(Some(discord_user_id)) => {
                let _ = crate::google::finish_oauth(&mut db, &returned);
                discord_user_id
            }
            Ok(None) => {
                return redirect(
                    &dest,
                    "That Google Calendar connection expired. Run /google authorize again.",
                );
            }
            Err(err) => return plain_error(err),
        }
    };
    let (refresh, access) = match crate::google::exchange_tokens(
        &state.http,
        &state.google_client_id,
        &state.google_client_secret,
        code,
        &redirect_uri,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(err) => return redirect(&dest, &err),
    };
    let email = crate::google::account_email(&state.http, &access).await;
    {
        let mut db = state.db.lock().expect("database lock");
        if let Err(err) = crate::google::save_account(&mut db, &discord_user_id, &refresh, &email) {
            return plain_error(err);
        }
    }
    let mut response = redirect(
        &dest,
        "Google Calendar is connected. Marian will add events you created and events you accepted, and update them when they change.",
    );
    clear_cookie(&mut response, "marian_google");
    response
}

async fn google_disconnect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NextForm>,
) -> Response {
    let next = form
        .next
        .as_deref()
        .and_then(auth::safe_next)
        .unwrap_or_else(|| "/".into());
    let session = {
        let mut db = state.db.lock().expect("database lock");
        read_session(&mut db, &headers)
    };
    let Some(session) = session else {
        return redirect("/", "Sign in with Discord.");
    };
    let access = {
        let mut db = state.db.lock().expect("database lock");
        match crate::google::remote_access(&mut db, &session.discord_user_id) {
            Ok(access) => access,
            Err(err) => return plain_error(err),
        }
    };
    let clean = if let Some(access) = access {
        if crate::google::configured(&state.google_client_id, &state.google_client_secret) {
            crate::google::forget_remote(
                &state.http,
                &state.google_client_id,
                &state.google_client_secret,
                access,
            )
            .await
        } else {
            false
        }
    } else {
        true
    };
    {
        let mut db = state.db.lock().expect("database lock");
        if let Err(err) = crate::google::delete_account(&mut db, &session.discord_user_id) {
            return plain_error(err);
        }
    }
    let notice = if clean {
        "Google Calendar is disconnected."
    } else {
        "Google Calendar is disconnected. Check it for any events that stayed behind."
    };
    redirect(&next, notice)
}

fn google_redirect(state: &AppState, headers: &HeaderMap) -> Option<String> {
    crate::google::redirect_uri(
        &state.public_url,
        headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok()),
        headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok()),
    )
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers, "marian_session") {
        let mut db = state.db.lock().expect("database lock");
        let _ = store::delete_web_session(&mut db, &token);
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
    let response = crate::observe::send_traced(
        state
            .http
            .post("https://discord.com/api/oauth2/token")
            .form(&[
                ("client_id", state.client_id.as_str()),
                ("client_secret", state.client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ]),
    )
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
    let response = crate::observe::send_traced(
        state
            .http
            .get("https://discord.com/api/users/@me")
            .bearer_auth(access),
    )
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

async fn discord_guilds(state: &AppState, access: &str) -> Result<Vec<MemberGuild>, String> {
    let mut found = Vec::new();
    let mut after = String::new();
    for _ in 0..5 {
        let mut endpoint = "https://discord.com/api/users/@me/guilds?limit=200".to_string();
        if !after.is_empty() {
            endpoint.push_str("&after=");
            endpoint.push_str(&auth::encode(&after));
        }
        let response = crate::observe::send_traced(state.http.get(endpoint).bearer_auth(access))
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
        found.extend(guilds.into_iter().map(|guild| MemberGuild {
            manages: auth::manages_guild(guild.owner, &guild.permissions),
            id: guild.id,
        }));
        if page < 200 {
            break;
        }
    }
    Ok(found)
}

#[derive(serde::Deserialize)]
struct NextQuery {
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct ListForm {
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: String,
}

impl ListForm {
    fn into_list(self) -> present::ListQuery {
        let page = self.page.trim().parse().unwrap_or(1);
        present::ListQuery::new(self.q, page)
    }
}

#[derive(serde::Deserialize)]
struct DiscordCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct GoogleCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct NextForm {
    next: Option<String>,
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
    #[serde(default)]
    owner: bool,
    #[serde(default)]
    permissions: String,
}

struct MemberGuild {
    id: String,
    manages: bool,
}

async fn drafts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Query(query): Query<ListForm>,
) -> Response {
    let query = query.into_list();
    html(&state, &headers, &server, "drafts", "Drafts", move |page| {
        present::list_page(page, "drafts", query)
    })
}

async fn scheduled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Query(query): Query<ListForm>,
) -> Response {
    let query = query.into_list();
    html(
        &state,
        &headers,
        &server,
        "scheduled",
        "Scheduled posts",
        move |page| present::list_page(page, "scheduled", query),
    )
}

async fn archive_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Query(query): Query<ListForm>,
) -> Response {
    let query = query.into_list();
    html(
        &state,
        &headers,
        &server,
        "archive",
        "Archive",
        move |page| present::list_page(page, "archive", query),
    )
}

async fn activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(&state, &headers, &server, "activity", "Activity", |page| {
        present::list_page(page, "activity", present::ListQuery::default())
    })
}

async fn recurring(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Query(query): Query<ListForm>,
) -> Response {
    let query = query.into_list();
    html(
        &state,
        &headers,
        &server,
        "recurring",
        "Recurring",
        move |page| present::recurring(page, query),
    )
}

async fn routines_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "routines",
        "Routines",
        present::routines_page,
    )
}

async fn new_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "routines",
        "New routine",
        present::routine_form,
    )
}

async fn show_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "routines",
        "Routine",
        move |page| present::routine_detail(page, &public_id),
    )
}

async fn create_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, "/routines/new");
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let parsed = routine_input(&map);
    let result = match parsed {
        Ok(input) => logic::create_routine(
            &mut db,
            session.guild_id,
            session.user_id,
            input.channel_id,
            &input.title,
            &input.prompt,
            &input.description,
            input.duration,
            input.start_minutes,
            input.poll_weekday,
            input.poll_minutes,
            input.lock_weekday,
            input.lock_minutes,
            &input.days,
            chrono::Utc::now(),
        ),
        Err(err) => Err(err),
    };
    match result {
        Ok((_id, public_id, jobs)) => {
            drop(db);
            for job in jobs {
                enqueue(&state.discord, job);
            }
            redirect(
                &here(&server, &format!("/routines/{public_id}")),
                "Saved the routine.",
            )
        }
        Err(err) => redirect(&back, &err.to_string()),
    }
}

async fn update_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, &format!("/routines/{public_id}"));
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let result = (|| {
        let id = logic::routine_in_guild(&mut db, session.guild_id, &public_id)?;
        let input = routine_input(&map)?;
        logic::save_routine(
            &mut db,
            id,
            session.user_id,
            input.channel_id,
            &input.title,
            &input.prompt,
            &input.description,
            input.duration,
            input.start_minutes,
            input.poll_weekday,
            input.poll_minutes,
            input.lock_weekday,
            input.lock_minutes,
            &input.days,
            chrono::Utc::now(),
        )
    })();
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

async fn save_routine_days(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, &format!("/routines/{public_id}"));
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let days = checked_days(&map);
    let result = (|| {
        let id = logic::routine_in_guild(&mut db, session.guild_id, &public_id)?;
        if map.get("scope").map(String::as_str) == Some("usual") {
            logic::save_usual_days(&mut db, id, session.user_id, &days)
                .map(|notice| (notice, Vec::new()))
        } else {
            logic::save_run_days(&mut db, id, session.user_id, &days)
                .map(|effect| (effect.notice, effect.jobs))
        }
    })();
    match result {
        Ok((notice, jobs)) => {
            drop(db);
            for job in jobs {
                enqueue(&state.discord, job);
            }
            redirect(&back, &notice)
        }
        Err(err) => redirect(&back, &err.to_string()),
    }
}

async fn vote_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, &format!("/routines/{public_id}"));
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let Some(day) = map.get("day").and_then(|value| value.parse().ok()) else {
        return redirect(&back, "Pick a day.");
    };
    let run_id = match logic::routine_in_guild(&mut db, session.guild_id, &public_id)
        .and_then(|id| logic::routine_page_data(&mut db, id))
    {
        Ok(Some(view)) => view.run.map(|run| run.id),
        Ok(None) => None,
        Err(err) => return redirect(&back, &err.to_string()),
    };
    let Some(run_id) = run_id else {
        return redirect(&back, "That poll is closed.");
    };
    match logic::cast_run(&mut db, run_id, session.user_id, day) {
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

async fn stop_routine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/routines/{public_id}")),
        move |db, session| {
            let id = logic::routine_in_guild(db, session.guild_id, &public_id)?;
            let notice = logic::stop_routine(db, id, session.user_id)?;
            Ok((notice, None))
        },
    )
}

struct RoutineInput {
    channel_id: i64,
    title: String,
    prompt: String,
    description: String,
    duration: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: Vec<u8>,
}

fn routine_input(map: &HashMap<String, String>) -> Result<RoutineInput, OpError> {
    let channel_id = map
        .get("channel")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| OpError::message("Pick a channel."))?;
    let duration = map
        .get("duration")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| OpError::message("Give the event a length in minutes."))?;
    Ok(RoutineInput {
        channel_id,
        title: map.get("title").cloned().unwrap_or_default(),
        prompt: map.get("prompt").cloned().unwrap_or_default(),
        description: map.get("description").cloned().unwrap_or_default(),
        duration,
        start_minutes: clock_minutes(map.get("start_time").map(String::as_str).unwrap_or(""))?,
        poll_weekday: weekday_field(map, "poll_weekday")?,
        poll_minutes: clock_minutes(map.get("poll_time").map(String::as_str).unwrap_or(""))?,
        lock_weekday: weekday_field(map, "lock_weekday")?,
        lock_minutes: clock_minutes(map.get("lock_time").map(String::as_str).unwrap_or(""))?,
        days: checked_days(map),
    })
}

fn weekday_field(map: &HashMap<String, String>, key: &str) -> Result<u8, OpError> {
    map.get(key)
        .and_then(|value| value.parse().ok())
        .filter(|day| *day < 7)
        .ok_or_else(|| OpError::message("Pick a weekday."))
}

fn clock_minutes(raw: &str) -> Result<u16, OpError> {
    let Some((hour, minute)) = raw.split_once(':') else {
        return Err(OpError::message("Pick a time."));
    };
    let hour: u16 = hour.parse().map_err(|_| OpError::message("Pick a time."))?;
    let minute: u16 = minute
        .parse()
        .map_err(|_| OpError::message("Pick a time."))?;
    if hour > 23 || minute > 59 {
        return Err(OpError::message("Pick a time."));
    }
    Ok(hour * 60 + minute)
}

fn checked_days(map: &HashMap<String, String>) -> Vec<u8> {
    (0..7)
        .filter(|day| map.contains_key(&format!("day_{day}")))
        .collect()
}

async fn series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Query(query): Query<ListForm>,
) -> Response {
    let query = query.into_list();
    html(
        &state,
        &headers,
        &server,
        "recurring",
        "Series",
        move |page| present::series_page(page, &public_id, query),
    )
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

async fn google_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
) -> Response {
    html(
        &state,
        &headers,
        &server,
        "google",
        "Google Calendar",
        present::google_page,
    )
}

async fn save_google(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server): Path<String>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, "/google");
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let notice = match commands::google_auto_sync(
        &mut db,
        session.guild_id,
        session.user_id,
        map.contains_key("auto_sync"),
    ) {
        Ok(notice) => notice,
        Err(err) => return redirect(&back, &err.to_string()),
    };
    drop(db);
    redirect(&back, &notice)
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
        let preset = query.get("preset").and_then(|raw| {
            let public_id = crate::ids::parse_event_id(raw)?;
            store::preset_by_public(page.conn, public_id)
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
            let draft = present::form_from_event(page.conn, event.id, tz)?;
            present::form_page(page, &draft)
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
    let Some(mut session) = read_session(&mut db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, server) {
        return guild_error(err);
    }
    let existing = match id.as_deref() {
        Some(public_id) => match store::event_by_public(&mut db, public_id) {
            Ok(Some(event)) if event.guild_id == session.guild_id => Some(event),
            Ok(_) => return render_missing(state, &mut db, &session),
            Err(err) => return plain_error(err),
        },
        None => None,
    };
    let guild = match store::guild(&mut db, session.guild_id) {
        Ok(Some(guild)) => guild,
        Ok(None) => return render_missing(state, &mut db, &session),
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
                &mut db,
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
            let saved = store::event(&mut db, event_id).ok().flatten();
            let public_id = saved
                .as_ref()
                .map(|event| event.public_id.clone())
                .unwrap_or_default();
            drop(db);
            enqueue(&state.discord, DiscordJob::sync(event_id));
            redirect(&here(server, &format!("/events/{public_id}")), "Saved.")
        }
        Err(OpError::NotFound) => render_missing(state, &mut db, &session),
        Err(err) => form_again(
            state,
            &mut db,
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
    db: &mut Connection,
    session: &Session,
    map: &HashMap<String, String>,
    existing: Option<&store::Event>,
    tz: chrono_tz::Tz,
    error: String,
) -> Response {
    let server = store::discord_server_id(db, session.guild_id).unwrap_or_default();
    let draft = present::form_from_map(map, &server, existing, error, tz);
    let mut page = page_for(state, db, session, "event", "Edit event");
    match present::form_page(&mut page, &draft) {
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
        let mut db = state.db.lock().expect("database lock");
        store::event_by_public(&mut db, &public_id)
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
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let id = match event_for_guild(&mut db, session.guild_id, &public_id) {
        Ok(event) => event.id,
        Err(OpError::NotFound) => return render_missing(&state, &mut db, &session),
        Err(err) => return plain_error(err),
    };
    match logic::duplicate_event(&mut db, id, session.user_id) {
        Ok(new_id) => {
            let copy = store::event(&mut db, new_id)
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

async fn save_series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
    Form(map): Form<HashMap<String, String>>,
) -> Response {
    let back = here(&server, &format!("/recurring/{public_id}"));
    mutate(&state, &headers, &server, &back, move |db, session| {
        let id = series_for_guild(db, session.guild_id, &public_id)?.id;
        let guild = store::guild(db, session.guild_id)?.ok_or(OpError::NotFound)?;
        let tz = clock::zone(&guild.timezone)?;
        let cadence = map.get("repeat").map(|value| value.trim()).unwrap_or("");
        let repeat_count = map
            .get("repeat_count")
            .map(|value| value.trim())
            .unwrap_or("");
        let repeat_until = map
            .get("repeat_until")
            .map(|value| value.trim())
            .unwrap_or("");
        let max_occurrences = if repeat_count.is_empty() {
            None
        } else {
            Some(
                repeat_count
                    .parse()
                    .map_err(|_| OpError::message("The date count needs to be a number."))?,
            )
        };
        let end_on = if repeat_until.is_empty() {
            None
        } else {
            Some(clock::parse_date(repeat_until, tz)?)
        };
        let notice = logic::save_series_repeat(
            db,
            id,
            session.user_id,
            logic::RepeatSpec {
                cadence: cadence.to_string(),
                end_on,
                max_occurrences,
            },
        )?;
        Ok((notice, None))
    })
}

async fn end_series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, &format!("/recurring/{public_id}")),
        move |db, session| {
            let id = series_for_guild(db, session.guild_id, &public_id)?.id;
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
    Path((server, public_id)): Path<(String, String)>,
) -> Response {
    mutate(
        &state,
        &headers,
        &server,
        &here(&server, "/presets"),
        move |db, session| {
            let Some(public_id) = crate::ids::parse_event_id(&public_id) else {
                return Err(OpError::NotFound);
            };
            let preset = store::preset_by_public(db, public_id)?.ok_or(OpError::NotFound)?;
            if preset.guild_id != session.guild_id {
                return Err(OpError::NotFound);
            }
            logic::delete_preset(db, preset.id, session.user_id)?;
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

fn library_option(conn: &mut Connection, guild_id: i64, name: &str) -> OptionInput {
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
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let line = map.get("line").cloned().unwrap_or_default();
    let reply = commands::execute(&mut db, session.guild_id, session.user_id, &line);
    let ids = events_to_refresh(&mut db, &line, &reply);
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
    let Some(mut session) = read_session(&mut db, &headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, &server) {
        return guild_error(err);
    }
    let user = match store::user(&mut db, session.user_id) {
        Ok(Some(user)) => user,
        _ => return redirect(&back, "Sign in again before changing settings."),
    };
    if !user.organizer {
        return redirect(
            &back,
            "Only someone who runs the server can change settings.",
        );
    }
    let mut settings = match store::settings(&mut db, session.guild_id) {
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
    if let Err(err) =
        diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(session.guild_id)))
            .set(schema::guilds::timezone.eq(&timezone))
            .execute(&mut *db)
    {
        return redirect(&back, &err.to_string());
    }
    if let Err(err) = store::update_settings(&mut db, &settings) {
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
    let mut db = state.db.lock().expect("database lock");
    let Some(guild) = store::guild_by_discord(&mut db, &server).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No server").into_response();
    };
    let Some(event) = calendar_event(&mut db, &file).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    };
    if event.guild_id != guild.id {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    }
    match ics::one_event(&mut db, event.id) {
        Ok(body) => calendar_response(body),
        Err(OpError::NotFound) => (StatusCode::NOT_FOUND, "No event").into_response(),
        Err(err) => plain_error(err),
    }
}

async fn legacy_event_calendar(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    if let Some(token) = personal_token(&file) {
        let Some(discord_id) = store::calendar_discord_id(&mut db, token).ok().flatten() else {
            return (StatusCode::NOT_FOUND, "No calendar").into_response();
        };
        return match ics::user_feed(&mut db, &discord_id) {
            Ok(body) => calendar_response(body),
            Err(err) => plain_error(err),
        };
    }
    let Some(event) = calendar_event(&mut db, &file).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    };
    match ics::one_event(&mut db, event.id) {
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

fn series_for_guild(
    conn: &mut Connection,
    guild_id: i64,
    public_id: &str,
) -> Result<store::Series, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Err(OpError::NotFound);
    };
    let series = store::series_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if series.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    Ok(series)
}

fn event_for_guild(
    conn: &mut Connection,
    guild_id: i64,
    public_id: &str,
) -> Result<store::Event, OpError> {
    let event = store::event_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if event.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    Ok(event)
}

fn calendar_event(conn: &mut Connection, file: &str) -> Result<Option<store::Event>, OpError> {
    let token = file.trim_end_matches(".ics");
    if let Some(public_id) = crate::ids::parse_event_id(token) {
        if let Some(event) = store::event_by_public(conn, public_id)? {
            return Ok(Some(event));
        }
    }
    Ok(None)
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
    let mut db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&mut db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    let server = match store::discord_server_id(&mut db, session.guild_id) {
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

async fn legacy_routines(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/routines")
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

async fn legacy_google(State(state): State<AppState>, headers: HeaderMap) -> Response {
    legacy_redirect(&state, &headers, "/google")
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
    let mut db = state.db.lock().expect("database lock");
    let Some(session) = read_session(&mut db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    let event = match calendar_event(&mut db, id) {
        Ok(Some(event)) => event,
        Ok(None) => return (StatusCode::NOT_FOUND, "No event").into_response(),
        Err(err) => return plain_error(err),
    };
    if !session.allowed.contains(&event.guild_id) {
        return (StatusCode::NOT_FOUND, "No event").into_response();
    }
    let server = match store::discord_server_id(&mut db, event.guild_id) {
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
    let Some(mut session) = read_session(&mut db, headers) else {
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
    render: impl FnOnce(&mut Page<'_>) -> Result<String, OpError>,
) -> Response {
    let mut db = state.db.lock().expect("database lock");
    let Some(mut session) = read_session(&mut db, headers) else {
        return redirect("/", "Sign in with Discord.");
    };
    if let Err(err) = bind_server(&mut db, &mut session, server) {
        return guild_error(err);
    }
    let mut page = page_for(state, &mut db, &session, nav, title);
    match render(&mut page) {
        Ok(body) => html_ok(body, session.notice.is_some()),
        Err(OpError::NotFound) => match present::not_found(&mut page) {
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

fn render_missing(state: &AppState, db: &mut Connection, session: &Session) -> Response {
    let mut page = page_for(state, db, session, "event", "Missing");
    match present::not_found(&mut page) {
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
    db: &'a mut Connection,
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
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
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

fn read_session(conn: &mut Connection, headers: &HeaderMap) -> Option<Session> {
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

pub fn events_to_refresh(conn: &mut Connection, line: &str, reply: &str) -> Vec<i64> {
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
    fn a_member_payload_is_the_current_role_list() {
        assert_eq!(server_in_path("/servers/99/settings"), Some("99"));
        assert_eq!(server_in_path("/servers/99"), Some("99"));
        assert_eq!(server_in_path("/settings"), None);
        match fresh_roles(StatusCode::OK, r#"{"roles":["222","nope",""]}"#) {
            FreshRoles::Known(roles) => assert_eq!(roles, vec!["222".to_string()]),
            FreshRoles::Unavailable => panic!("expected the role ids"),
        }
        match fresh_roles(StatusCode::NOT_FOUND, "") {
            FreshRoles::Known(roles) => assert!(roles.is_empty()),
            FreshRoles::Unavailable => panic!("a missing member has no roles"),
        }
        assert!(matches!(
            fresh_roles(StatusCode::TOO_MANY_REQUESTS, ""),
            FreshRoles::Unavailable
        ));
        assert!(matches!(
            fresh_roles(StatusCode::OK, "not-json"),
            FreshRoles::Unavailable
        ));
    }

    #[test]
    fn notice_round_trip_and_event_ids() {
        let encoded = percent_encode("Posted Shore supper.");
        assert_eq!(percent_decode(&encoded), "Posted Shore supper.");
        let ids = mentioned_events("event:12", "Open /events/12. Updated event 4.");
        assert_eq!(ids, vec![4, 12]);
    }

    #[test]
    fn the_span_keeps_the_user_agent_and_the_client_address() {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, "MarianTest/1".parse().unwrap());
        assert_eq!(user_agent(&headers).as_deref(), Some("MarianTest/1"));
        headers.insert(header::USER_AGENT, "  ".parse().unwrap());
        assert_eq!(user_agent(&headers), None);
        let long = "a".repeat(300);
        headers.insert(header::USER_AGENT, HeaderValue::from_str(&long).unwrap());
        assert_eq!(user_agent(&headers).unwrap().chars().count(), 256);

        let peer = "10.0.0.8:443".parse().unwrap();
        headers.insert("x-real-ip", "203.0.113.5".parse().unwrap());
        headers.insert("x-forwarded-for", "1.2.3.4, 203.0.113.5".parse().unwrap());
        assert_eq!(
            client_ip(&headers, Some(peer)).as_deref(),
            Some("203.0.113.5")
        );

        headers.remove("x-real-ip");
        assert_eq!(client_ip(&headers, Some(peer)).as_deref(), Some("1.2.3.4"));

        headers.insert(
            "x-forwarded-for",
            "not-an-ip, 198.51.100.20".parse().unwrap(),
        );
        assert_eq!(
            client_ip(&headers, Some(peer)).as_deref(),
            Some("198.51.100.20")
        );

        headers.insert("x-forwarded-for", "[2001:db8::1]:443".parse().unwrap());
        assert_eq!(
            client_ip(&headers, Some(peer)).as_deref(),
            Some("2001:db8::1")
        );

        headers.remove("x-forwarded-for");
        assert_eq!(client_ip(&headers, Some(peer)).as_deref(), Some("10.0.0.8"));
        let mapped = "[::ffff:127.0.0.1]:9".parse().unwrap();
        assert_eq!(
            client_ip(&headers, Some(mapped)).as_deref(),
            Some("127.0.0.1")
        );
    }

    #[tokio::test]
    async fn sign_in_is_required() {
        let mut conn = store::open_memory().unwrap();
        diesel::insert_into(schema::guilds::table)
            .values((
                schema::guilds::id.eq(1),
                schema::guilds::name.eq("Guild"),
                schema::guilds::timezone.eq("UTC"),
                schema::guilds::discord_id.eq("99"),
            ))
            .execute(&mut conn)
            .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let user_id = diesel::insert_into(schema::users::table)
            .values((
                schema::users::guild_id.eq(1),
                schema::users::name.eq("Nell"),
                schema::users::handle.eq("nell"),
                schema::users::color.eq("#1e3a6e"),
                schema::users::reminders_enabled.eq(true),
                schema::users::organizer.eq(true),
                schema::users::discord_id.eq("42"),
            ))
            .returning(schema::users::id)
            .get_result::<i64>(&mut conn)
            .unwrap();
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
            google_client_id: String::new(),
            google_client_secret: String::new(),
            bot_token: String::new(),
        };
        let app = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let home = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(home.status(), StatusCode::OK);
        assert_eq!(
            home.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let body = home.text().await.unwrap();
        assert!(body.contains("Sign in with Discord"));
        assert!(body.contains("class=\"gate-actions\""));
        assert!(body.contains("Invite Marian"));
        assert!(body.contains("Events for your server"));
        let mark = assets::mark();
        assert!(body.contains(mark), "{body}");

        let picture = client
            .get(format!("http://{addr}{mark}"))
            .send()
            .await
            .unwrap();
        assert_eq!(picture.status(), StatusCode::OK);
        assert_eq!(
            picture.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(
            picture.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            picture
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );
        let etag = picture
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let cached = client
            .get(format!("http://{addr}{mark}"))
            .header(header::IF_NONE_MATCH, &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
        let missing = client
            .get(format!("http://{addr}/static/missing.css"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

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
        assert!(body.contains("Google Calendar"));
        assert!(!body.contains("Google client id"), "{body}");
        assert!(body.contains("class=\"calendar\""));
        assert!(body.contains(".ics"));
        assert!(body.contains("Sign out"));
        assert!(body.contains("class=\"invite\""));
        assert!(body.contains("Invite Marian to Discord"));
        assert!(body.contains(assets::mark()), "{body}");
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

        let google = client
            .get(format!("http://{addr}/servers/99/google"))
            .header(header::COOKIE, "marian_session=session-token-value")
            .send()
            .await
            .unwrap();
        assert_eq!(google.status(), StatusCode::OK);
        let google_body = google.text().await.unwrap();
        assert!(google_body.contains("Google client id"), "{google_body}");
        assert!(google_body.contains("servers&#x2f;99/google"), "{google_body}");

        let legacy_google = client
            .get(format!("http://{addr}/google"))
            .header(header::COOKIE, "marian_session=session-token-value")
            .send()
            .await
            .unwrap();
        assert_eq!(legacy_google.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            legacy_google.headers().get(header::LOCATION).unwrap(),
            "/servers/99/google"
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

    #[tokio::test]
    async fn google_callback_keeps_a_link_the_browser_did_not_start() {
        let mut conn = store::open_memory().unwrap();
        let token = crate::google::begin_link(&mut conn, "42").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let db = Arc::new(Mutex::new(conn));
        let state = AppState {
            db: db.clone(),
            discord: tx,
            invite_url: String::new(),
            http: reqwest::Client::new(),
            client_id: String::new(),
            client_secret: String::new(),
            public_url: "https://marian.example".into(),
            google_client_id: "google-client".into(),
            google_client_secret: "google-secret".into(),
            bot_token: String::new(),
        };
        let app = router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let missed = client
            .get(format!(
                "http://{addr}/auth/google/callback?state={token}&error=access_denied"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(missed.status(), StatusCode::SEE_OTHER);
        let still = {
            let mut guard = db.lock().unwrap();
            crate::google::claim_oauth(&mut guard, &token).unwrap()
        };
        assert_eq!(still.as_deref(), Some("42"));

        let open = client
            .get(format!("http://{addr}/auth/google/start/{token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(open.status(), StatusCode::SEE_OTHER);
        let location = open
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(location.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(location.contains(&token));
        let set = open
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set.starts_with(&format!("marian_google={token}.")));
    }
}
