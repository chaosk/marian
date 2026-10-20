use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Form, MatchedPath, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use diesel::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::assets;
use crate::auth;
use crate::error::OpError;
use crate::ics;
use crate::jobs::DiscordJob;
use crate::logic;
use crate::store::{self, Connection};

mod report;
mod rest;

#[derive(Clone)]
struct AppState {
    db: store::Db,
    discord: UnboundedSender<DiscordJob>,
    invite_url: String,
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    public_url: String,
    google_client_id: String,
    google_client_secret: String,
    bot_token: String,
    member_roles: Arc<Mutex<MemberRoles>>,
}

use crate::api::{cookie, Viewer as Session};

/// Runs Marian: the gateway, the tick, and the web service with `pages` for everything else.
pub fn run(pages: impl FnOnce(crate::api::Web) -> Router + Send + 'static) -> anyhow::Result<()> {
    store::load_dotenv();
    let _telemetry = crate::observe::init();

    let database_url = store::database_url()?;
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

    let db = store::pool(&database_url)?;

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
        member_roles: member_roles_cache(),
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let web = crate::api::Web {
            db: state.db.clone(),
            invite_url: state.invite_url.clone(),
            oauth_ready: oauth_ready(&state),
        };
        let app = router(state.clone(), pages(web));
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
                    let mut guard = db.get().expect("database");
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

fn router(state: AppState, pages: Router) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/favicon.svg", get(asset))
        .route("/static/{*file}", get(asset))
        .route("/auth/discord", get(auth_start))
        .route("/auth/discord/callback", get(auth_callback))
        .route("/auth/google", get(google_start))
        .route("/auth/google/start/{state}", get(google_open))
        .route("/auth/google/callback", get(google_callback))
        .route("/auth/google/disconnect", post(google_disconnect))
        .route("/logout", post(logout))
        .route("/calendar/{file}", get(personal_calendar))
        .nest("/api", rest::router())
        .with_state(state.clone())
        .merge(pages)
        .layer(middleware::from_fn(revalidate_site))
        .layer(middleware::from_fn_with_state(state, require_login))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(middleware::from_fn(mark_page_trace))
        .layer(http_trace())
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

/// Hands a page render the trace it runs in. The shell writes it into the
/// page, and a browser error report sends it back.
async fn mark_page_trace(mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let page = request.method() == axum::http::Method::GET
        && !path.starts_with("/api/")
        && !path.starts_with("/pkg/");
    if page {
        if let Some(trace) = crate::observe::traceparent() {
            request
                .extensions_mut()
                .insert(crate::observe::PageTrace(trace));
        }
    }
    next.run(request).await
}

fn quiet_path(path: &str) -> bool {
    path == "/healthz"
        || path == "/readyz"
        || path == "/favicon.svg"
        || path.starts_with("/static/")
        || path.ends_with("/live")
}

/// The site's script, wasm, and stylesheet. A release build made with
/// `--precompress` leaves `.br` and `.gz` copies beside each file, compressed
/// once at the highest setting; a browser that accepts one gets it as is, and
/// the compression layer leaves it alone. Without the copies (a debug build)
/// the plain file goes out and is compressed on the way.
pub fn site_files(dir: impl AsRef<std::path::Path>) -> tower_http::services::ServeDir {
    tower_http::services::ServeDir::new(dir)
        .precompressed_br()
        .precompressed_gzip()
}

/// The script and its wasm keep fixed names across builds, so a browser that
/// guesses one is still fresh can pair it with the other from a newer build and
/// fail to start. Making it ask each time keeps the pair together. A file that
/// hasn't changed since the browser's copy is answered with 304 here, in case
/// whatever served it didn't look at `If-Modified-Since`.
async fn revalidate_site(request: Request, next: Next) -> Response {
    if !request.uri().path().starts_with("/pkg/") {
        return next.run(request).await;
    }
    let since = request.headers().get(header::IF_MODIFIED_SINCE).cloned();
    let mut response = next.run(request).await;
    let unchanged = response.status() == StatusCode::OK
        && since.is_some()
        && response.headers().get(header::LAST_MODIFIED) == since.as_ref();
    if unchanged {
        let mut fresh = StatusCode::NOT_MODIFIED.into_response();
        if let Some(modified) = response.headers().get(header::LAST_MODIFIED) {
            fresh.headers_mut().insert(header::LAST_MODIFIED, modified.clone());
        }
        response = fresh;
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

async fn health() -> &'static str {
    "ok"
}

async fn ready(State(state): State<AppState>) -> Response {
    let ok = state.db.get().map(|mut conn| {
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

fn read_session(conn: &mut Connection, headers: &HeaderMap) -> Option<Session> {
    crate::api::viewer(conn, headers)
}

fn oauth_ready(state: &AppState) -> bool {
    !state.client_id.trim().is_empty() && !state.client_secret.trim().is_empty()
}

/// Asks for Discord sign-in before server pages and the REST API, and keeps roles fresh.
async fn require_login(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let full = request.uri().path().to_string();
    let api = full
        .strip_prefix("/api")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'));
    let path = match api {
        Some("") => "/".to_string(),
        Some(rest) => rest.to_string(),
        None => full.clone(),
    };
    let gated = match api {
        Some(_) => !auth::is_public(&path),
        None => path.starts_with("/servers/") || path == "/servers",
    };
    if !gated {
        return next.run(request).await;
    }
    let headers = request.headers().clone();
    let signed_in = {
        let mut db = state.db.get().expect("database");
        read_session(&mut db, &headers).is_some()
    };
    if signed_in {
        refresh_member_roles(&state, &path, &headers).await;
        return next.run(request).await;
    }
    let target = match request.uri().query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let login = auth::login_location(&target);
    if api.is_some() {
        let body = marian_types::Problem {
            error: "Sign in with Discord.".into(),
            redirect: Some(login),
        };
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }
    nav_redirect(&login, "Sign in with Discord.")
}

struct RoleTarget {
    guild_id: i64,
    membership_id: i64,
    guild_discord: String,
    user_discord: String,
}

const MEMBER_ROLES_TTL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct MemberRoles {
    fetched: HashMap<(String, String), std::time::Instant>,
}

fn member_roles_cache() -> Arc<Mutex<MemberRoles>> {
    Arc::new(Mutex::new(MemberRoles::default()))
}

impl MemberRoles {
    fn fresh(&self, guild: &str, user: &str, now: std::time::Instant) -> bool {
        self.fetched
            .get(&member_key(guild, user))
            .is_some_and(|at| now.saturating_duration_since(*at) < MEMBER_ROLES_TTL)
    }

    fn remember(&mut self, guild: &str, user: &str, now: std::time::Instant) {
        let ttl = MEMBER_ROLES_TTL;
        self.fetched
            .retain(|_, at| now.saturating_duration_since(*at) < ttl);
        self.fetched.insert(member_key(guild, user), now);
    }

    fn forget_user(&mut self, user: &str) {
        self.fetched.retain(|(_, id), _| id != user);
    }
}

fn member_key(guild: &str, user: &str) -> (String, String) {
    (guild.to_string(), user.to_string())
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
    if state
        .member_roles
        .lock()
        .expect("member roles")
        .fresh(&target.guild_discord, &target.user_discord, std::time::Instant::now())
    {
        return;
    }
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
    state
        .member_roles
        .lock()
        .expect("member roles")
        .remember(
            &target.guild_discord,
            &target.user_discord,
            std::time::Instant::now(),
        );
}

fn load_role_target(state: &AppState, path: &str, headers: &HeaderMap) -> Option<RoleTarget> {
    let server = server_in_path(path)?;
    let mut db = state.db.get().expect("database");
    role_target(&mut db, headers, server)
}

fn store_member_roles(state: &AppState, target: &RoleTarget, roles: &[String]) {
    let mut db = state.db.get().expect("database");
    if let Err(err) = store::replace_discord_roles(&mut db, target.guild_id, target.membership_id, roles)
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
        membership_id: user.id,
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
            "https://discord.com/api/v10/guilds/{guild_discord}/members/{user_discord}"
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
        return nav_redirect("/", "Sign-in is not configured yet.");
    }
    let Some(redirect_uri) = oauth_redirect(&state, &headers) else {
        return nav_redirect("/", "Set MARIAN_PUBLIC_URL so Discord can send you back.");
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
        return nav_redirect("/", "Sign-in was cancelled.");
    }
    let Some(code) = query.code.as_deref().filter(|code| !code.is_empty()) else {
        return nav_redirect("/", "Discord didn't send a sign-in code.");
    };
    let Some(returned) = query.state.as_deref() else {
        return nav_redirect("/", "That sign-in expired. Try again.");
    };
    let Some((expected, next)) =
        cookie(&headers, "marian_oauth").and_then(|raw| auth::unpack_oauth(&raw))
    else {
        return nav_redirect("/", "That sign-in expired. Try again.");
    };
    if returned != expected {
        return nav_redirect("/", "That sign-in expired. Try again.");
    }
    let Some(redirect_uri) = oauth_redirect(&state, &headers) else {
        return nav_redirect("/", "Set MARIAN_PUBLIC_URL so Discord can send you back.");
    };
    let access = match discord_token(&state, code, &redirect_uri).await {
        Ok(token) => token,
        Err(err) => return nav_redirect("/", &err),
    };
    let me = match discord_me(&state, &access).await {
        Ok(me) => me,
        Err(err) => return nav_redirect("/", &err),
    };
    let member_of = match discord_guilds(&state, &access).await {
        Ok(ids) => ids,
        Err(err) => return nav_redirect("/", &err),
    };
    let mut db = state.db.get().expect("database");
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
        return nav_redirect("/", "Marian isn't in a server you belong to yet.");
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
        if let Err(err) = logic::set_server_manager(&mut db, user.id, manages) {
            return plain_error(err);
        }
        if signed_in.is_none() {
            signed_in = Some((guild_id, user.id));
        }
    }
    let Some((guild_id, membership_id)) = signed_in else {
        return nav_redirect("/", "Marian isn't in a server you belong to yet.");
    };
    let token = match auth::random_token() {
        Ok(token) => token,
        Err(err) => return plain_error(err),
    };
    if let Err(err) =
        store::insert_web_session(&mut db, &token, &me.id, &name, guild_id, membership_id, &allowed)
    {
        return plain_error(err);
    }
    drop(db);
    state
        .member_roles
        .lock()
        .expect("member roles")
        .forget_user(&me.id);
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
    db: &store::Db,
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    public_base: &str,
) {
    if !crate::google::configured(client_id, client_secret) {
        return;
    }
    let jobs = {
        let mut guard = db.get().expect("database");
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
        let mut guard = db.get().expect("database");
        if let Err(err) = crate::google::record(&mut guard, outcome) {
            tracing::warn!("google calendar: {err}");
        }
    }
    let revokes = {
        let mut guard = db.get().expect("database");
        match crate::google::ready_revokes(&mut guard) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("google calendar: {err}");
                Vec::new()
            }
        }
    };
    for access in revokes {
        let user_id = access.user_id;
        if crate::google::forget_remote(http, client_id, client_secret, access).await {
            let mut guard = db.get().expect("database");
            if let Err(err) = crate::google::delete_account(&mut guard, user_id) {
                tracing::warn!("google calendar: {err}");
            }
        } else {
            tracing::warn!("google calendar disconnect for {user_id} will retry");
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
        return nav_redirect(&next, "Google Calendar is not configured yet.");
    }
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return nav_redirect(&next, "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let session = {
        let mut db = state.db.get().expect("database");
        read_session(&mut db, &headers)
    };
    let Some(session) = session else {
        return nav_redirect(&next, "Sign in with Discord.");
    };
    let state_token = {
        let mut db = state.db.get().expect("database");
        match crate::google::begin_link(&mut db, session.user_id) {
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
        return nav_redirect(
            "/",
            "That Google Calendar connection expired. Run /google authorize again.",
        );
    }
    if !crate::google::configured(&state.google_client_id, &state.google_client_secret) {
        return nav_redirect("/", "Google Calendar is not configured yet.");
    }
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return nav_redirect("/", "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let pending = {
        let mut db = state.db.get().expect("database");
        match crate::google::claim_oauth(&mut db, &token) {
            Ok(row) => row.is_some(),
            Err(err) => return plain_error(err),
        }
    };
    if !pending {
        return nav_redirect(
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
        return nav_redirect(
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
        let mut db = state.db.get().expect("database");
        let _ = crate::google::finish_oauth(&mut db, &returned);
        let mut response = nav_redirect(
            &dest,
            "The Google Calendar connection stopped before it finished.",
        );
        clear_cookie(&mut response, "marian_google");
        return response;
    }
    let Some(code) = query.code.as_deref().filter(|code| !code.is_empty()) else {
        return nav_redirect(&dest, "Google didn't send a connection code.");
    };
    let Some(redirect_uri) = google_redirect(&state, &headers) else {
        return nav_redirect(&dest, "Set MARIAN_PUBLIC_URL so Google can send you back.");
    };
    let user_id = {
        let mut db = state.db.get().expect("database");
        match crate::google::claim_oauth(&mut db, &returned) {
            Ok(Some(user_id)) => {
                let _ = crate::google::finish_oauth(&mut db, &returned);
                user_id
            }
            Ok(None) => {
                return nav_redirect(
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
        Err(err) => return nav_redirect(&dest, &err),
    };
    let email = crate::google::account_email(&state.http, &access).await;
    {
        let mut db = state.db.get().expect("database");
        if let Err(err) = crate::google::save_account(&mut db, user_id, &refresh, &email) {
            return plain_error(err);
        }
    }
    let mut response = nav_redirect(
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
        let mut db = state.db.get().expect("database");
        read_session(&mut db, &headers)
    };
    let Some(session) = session else {
        return nav_redirect("/", "Sign in with Discord.");
    };
    let access = {
        let mut db = state.db.get().expect("database");
        match crate::google::remote_access(&mut db, session.user_id) {
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
        let mut db = state.db.get().expect("database");
        if let Err(err) = crate::google::delete_account(&mut db, session.user_id) {
            return plain_error(err);
        }
    }
    let notice = if clean {
        "Google Calendar is disconnected."
    } else {
        "Google Calendar is disconnected. Check it for any events that stayed behind."
    };
    nav_redirect(&next, notice)
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
        let mut db = state.db.get().expect("database");
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
            .get("https://discord.com/api/v10/users/@me")
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
        let mut endpoint = "https://discord.com/api/v10/users/@me/guilds?limit=200".to_string();
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
        let body = response
            .text()
            .await
            .map_err(|_| "Discord sent an unexpected server list.".to_string())?;
        let guilds = member_guilds(&body).map_err(|err| {
            tracing::warn!("discord guild list did not match: {err}");
            "Discord sent an unexpected server list.".to_string()
        })?;
        let page = guilds.len();
        if let Some(last) = guilds.last() {
            after = last.id.clone();
        }
        found.extend(guilds);
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
    #[serde(default, deserialize_with = "permission_bits")]
    permissions: String,
}

fn permission_bits<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bits;
    impl serde::de::Visitor<'_> for Bits {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("permission bits as a string or a number")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
    }
    deserializer.deserialize_any(Bits)
}

fn member_guilds(body: &str) -> Result<Vec<MemberGuild>, serde_json::Error> {
    let guilds: Vec<DiscordGuild> = serde_json::from_str(body)?;
    Ok(guilds
        .into_iter()
        .map(|guild| MemberGuild {
            manages: auth::manages_guild(guild.owner, &guild.permissions),
            id: guild.id,
        })
        .collect())
}

struct MemberGuild {
    id: String,
    manages: bool,
}

async fn personal_calendar(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    let Some(token) = personal_token(&file) else {
        return (StatusCode::NOT_FOUND, "No calendar").into_response();
    };
    let mut db = state.db.get().expect("database");
    let Some(user_id) = store::calendar_user_id(&mut db, token).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "No calendar").into_response();
    };
    match ics::user_feed(&mut db, user_id) {
        Ok(body) => calendar_response(body),
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

fn error_json(status: StatusCode, text: &str) -> Response {
    (status, Json(serde_json::json!({ "error": text }))).into_response()
}

fn plain_error(err: OpError) -> Response {
    error_json(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())
}

fn safe_path(path: &str) -> &str {
    if path.starts_with('/') && !path.starts_with("//") {
        path
    } else {
        "/"
    }
}

/// A browser redirect for sign-in and sign-out, which leave the app.
fn nav_redirect(path: &str, notice: &str) -> Response {
    let path = safe_path(path);
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

fn invite_url(client_id: &str) -> String {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return String::new();
    }
    format!(
        "https://discord.com/api/oauth2/authorize?client_id={client_id}&permissions=326686174288&scope=bot%20applications.commands"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::percent_decode;
    use crate::schema;

    #[test]
    fn a_server_list_accepts_string_and_integer_permissions() {
        let current = r#"[{"id":"1","owner":false,"permissions":"40"}]"#;
        let legacy = r#"[{"id":"2","owner":false,"permissions":8,"permissions_new":"8"}]"#;
        let parsed = member_guilds(current).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].manages);
        let parsed = member_guilds(legacy).unwrap();
        assert_eq!(parsed[0].id, "2");
        assert!(parsed[0].manages);
        let plain = member_guilds(r#"[{"id":"3","owner":false,"permissions":0}]"#).unwrap();
        assert!(!plain[0].manages);
    }

    #[derive(Insertable)]
    #[diesel(table_name = schema::guilds)]
    struct SeedGuild<'a> {
        id: i64,
        name: &'a str,
        timezone: &'a str,
        discord_id: &'a str,
    }

    #[derive(Insertable)]
    #[diesel(table_name = schema::users)]
    struct SeedUser<'a> {
        discord_id: &'a str,
    }

    #[derive(Insertable)]
    #[diesel(table_name = schema::memberships)]
    struct SeedMembership<'a> {
        user_id: i64,
        guild_id: i64,
        name: &'a str,
        handle: &'a str,
        color: &'a str,
        reminders_enabled: bool,
        server_manager: bool,
    }

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
    fn a_member_lookup_is_reused_for_thirty_seconds() {
        let now = std::time::Instant::now();
        let mut roles = MemberRoles::default();
        assert!(!roles.fresh("99", "42", now));
        roles.remember("99", "42", now);
        assert!(roles.fresh("99", "42", now + std::time::Duration::from_secs(29)));
        assert!(!roles.fresh("99", "42", now + MEMBER_ROLES_TTL));
        roles.remember("7", "42", now);
        roles.forget_user("42");
        assert!(!roles.fresh("99", "42", now));
        assert!(!roles.fresh("7", "42", now));
        roles.remember("99", "42", now - MEMBER_ROLES_TTL);
        roles.remember("99", "8", now);
        assert_eq!(roles.fetched.len(), 1);
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
    async fn site_files_go_out_precompressed_when_the_browser_takes_it() {
        let dir = std::env::temp_dir().join(format!("marian-pkg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = "fn main() {} ".repeat(400);
        std::fs::write(dir.join("marian.wasm"), &plain).unwrap();
        std::fs::write(dir.join("marian.wasm.br"), b"brotli bytes").unwrap();

        let mut conn = store::open_memory().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState {
            db: store::pool_for(&mut conn).unwrap(),
            discord: tx,
            invite_url: String::new(),
            http: reqwest::Client::new(),
            client_id: String::new(),
            client_secret: String::new(),
            public_url: String::new(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            bot_token: String::new(),
            member_roles: member_roles_cache(),
        };
        let app = router(state, Router::new().nest_service("/pkg", site_files(&dir)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .unwrap();
        });
        // No automatic decoding, so the bytes on the wire are what's checked.
        let client = reqwest::Client::builder().no_gzip().no_brotli().build().unwrap();
        let url = format!("http://{addr}/pkg/marian.wasm");
        let fetch = |encoding: &'static str| {
            let request = client.get(&url).header(header::ACCEPT_ENCODING, encoding);
            async move { request.send().await.unwrap() }
        };

        let br = fetch("gzip, br").await;
        assert_eq!(br.headers().get(header::CONTENT_ENCODING).unwrap(), "br");
        assert_eq!(br.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        let modified = br.headers().get(header::LAST_MODIFIED).cloned().unwrap();
        assert_eq!(br.bytes().await.unwrap().as_ref(), b"brotli bytes", "the stored copy, not compressed again");

        // No .gz copy here: the plain file is compressed on the way instead.
        let gzip = fetch("gzip").await;
        assert_eq!(gzip.headers().get(header::CONTENT_ENCODING).unwrap(), "gzip");
        assert!(gzip.bytes().await.unwrap().len() < plain.len());

        let identity = fetch("identity").await;
        assert!(identity.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(identity.text().await.unwrap(), plain);

        let again = client
            .get(&url)
            .header(header::ACCEPT_ENCODING, "br")
            .header(header::IF_MODIFIED_SINCE, modified)
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn sign_in_is_required() {
        let mut conn = store::open_memory().unwrap();
        diesel::insert_into(schema::guilds::table)
            .values(&SeedGuild {
                id: 1,
                name: "Guild",
                timezone: "UTC",
                discord_id: "99",
            })
            .execute(&mut conn)
            .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let user_id = diesel::insert_into(schema::users::table)
            .values(&SeedUser { discord_id: "42" })
            .returning(schema::users::id)
            .get_result::<i64>(&mut conn)
            .unwrap();
        let membership_id = diesel::insert_into(schema::memberships::table)
            .values(&SeedMembership {
                user_id,
                guild_id: 1,
                name: "Nell",
                handle: "nell",
                color: "#1e3a6e",
                reminders_enabled: true,
                server_manager: true,
            })
            .returning(schema::memberships::id)
            .get_result::<i64>(&mut conn)
            .unwrap();
        store::insert_web_session(
            &mut conn,
            "session-token-value",
            "42",
            "Nell",
            1,
            membership_id,
            &[1],
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState {
            db: store::pool_for(&mut conn).unwrap(),
            discord: tx,
            invite_url: "https://discord.com/oauth2/authorize?example".into(),
            http: reqwest::Client::new(),
            client_id: "123".into(),
            client_secret: "test-secret".into(),
            public_url: "http://127.0.0.1:8080".into(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            bot_token: String::new(),
            member_roles: member_roles_cache(),
        };
        let app = router(state, Router::new());
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
        let deep = client
            .get(format!("http://{addr}/servers/99/settings"))
            .send()
            .await
            .unwrap();
        assert_eq!(deep.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            deep.headers().get(header::LOCATION).unwrap(),
            "/?next=%2Fservers%2F99%2Fsettings"
        );

        let mark = assets::mark();
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
            .get(format!("http://{addr}/api/servers/99/settings"))
            .send()
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);
        let blocked: serde_json::Value = blocked.json().await.unwrap();
        assert_eq!(blocked["redirect"], "/?next=%2Fservers%2F99%2Fsettings");

        let signed = |request: reqwest::RequestBuilder| {
            request.header(header::COOKIE, "marian_session=session-token-value")
        };
        let me: marian_types::Me = signed(client.get(format!("http://{addr}/api/me")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me.name, "Nell");
        assert_eq!(me.last_server.as_deref(), Some("99"));
        assert_eq!(me.servers[0].name, "Guild");

        let membership: marian_types::Membership =
            signed(client.get(format!("http://{addr}/api/servers/99/me")))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(membership.name, "Nell");
        assert!(membership.reminders_enabled);
        assert!(membership.calendar_url.ends_with(".ics"));
        assert!(!membership.google.ready);

        let open = signed(client.get(format!("http://{addr}/api/servers/99/settings")))
            .send()
            .await
            .unwrap();
        assert_eq!(open.status(), StatusCode::OK);
        let mut settings: marian_types::Settings = open.json().await.unwrap();
        assert_eq!(settings.timezone, "UTC");
        settings.hour_24 = !settings.hour_24;
        settings.timezone = "Europe/Warsaw".into();
        let saved: marian_types::Done =
            signed(client.put(format!("http://{addr}/api/servers/99/settings")))
                .json(&settings)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert!(saved.location.is_none());
        let server: marian_types::Server =
            signed(client.get(format!("http://{addr}/api/servers/99")))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(server.timezone, "Europe/Warsaw");
        assert_eq!(server.hour_24, settings.hour_24);

        settings.default_reminder_minutes = -1;
        let refused = signed(client.put(format!("http://{addr}/api/servers/99/settings")))
            .json(&settings)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let drafts: marian_types::Paged<marian_types::EventSummary> =
            signed(client.get(format!("http://{addr}/api/servers/99/events?view=drafts")))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(drafts.total, 0);

        let elsewhere = signed(client.get(format!("http://{addr}/api/servers/77/settings")))
            .send()
            .await
            .unwrap();
        assert_eq!(elsewhere.status(), StatusCode::NOT_FOUND);

        let missing = signed(client.get(format!("http://{addr}/api/servers/99/nowhere")))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

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
        let user_id = store::ensure_user(&mut conn, "42").unwrap();
        let token = crate::google::begin_link(&mut conn, user_id).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let db = store::pool_for(&mut conn).unwrap();
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
            member_roles: member_roles_cache(),
        };
        let app = router(state, Router::new());
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
            let mut guard = db.get().unwrap();
            crate::google::claim_oauth(&mut guard, &token).unwrap()
        };
        assert_eq!(still, Some(user_id));

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
