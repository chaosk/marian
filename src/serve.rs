use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Form, MatchedPath, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use chrono::Utc;
use diesel::prelude::*;
use tokio::sync::mpsc::UnboundedSender;
use tracing::Instrument;

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
    /// The bot's member list, when it can vouch for members' roles.
    members: crate::gateway::MemberView,
    /// When the task worker last finished a pass, for `/healthz`.
    heartbeat: Heartbeat,
    /// Whether this replica leads, and so runs the task worker.
    leader: crate::leader::Status,
}

/// The last time the task worker finished a pass, in Unix seconds. It starts at
/// launch, so a fresh process has the full grace period before it counts as
/// stuck.
#[derive(Clone)]
struct Heartbeat(Arc<AtomicI64>);

/// How long the task worker may go without finishing a pass before
/// `/healthz` fails and Kubernetes restarts the process. It finishes one at
/// least every minute (`TASKS_IDLE`), even when nothing is due.
const TASKS_STALE_AFTER_SECS: i64 = 120;

impl Heartbeat {
    fn new() -> Self {
        Self(Arc::new(AtomicI64::new(Utc::now().timestamp())))
    }

    fn beat(&self) {
        self.0.store(Utc::now().timestamp(), Ordering::Relaxed);
    }

    fn stale(&self, now: i64) -> bool {
        now - self.0.load(Ordering::Relaxed) > TASKS_STALE_AFTER_SECS
    }
}

/// Runs `body` with a pooled connection on a blocking thread, so database
/// work doesn't hold up the async workers. `None`, after a warning, when no
/// connection came or the work panicked; the caller skips this round.
async fn with_db<T: Send + 'static>(
    db: &store::Db,
    what: &'static str,
    body: impl FnOnce(&mut Connection) -> T + Send + 'static,
) -> Option<T> {
    let db = db.clone();
    match tokio::task::spawn_blocking(move || db.get().map(|mut conn| body(&mut conn))).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(err)) => {
            tracing::warn!("{what}: no database connection, skipping this round: {err}");
            None
        }
        Err(err) => {
            tracing::warn!("{what}: stopped unexpectedly, skipping this round: {err}");
            None
        }
    }
}

/// The longest the task worker sleeps, so `/healthz` hears from it at least
/// this often even when nothing is due for hours.
const TASKS_IDLE: std::time::Duration = std::time::Duration::from_secs(60);
/// How many tasks one pass runs before it reports and starts the next.
const TASKS_PER_PASS: usize = 16;
/// How long the worker waits after a pass that couldn't reach the database.
const TASKS_BACK_OFF: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs posts, reminders, signup cutoffs, archiving, series dates, routines
/// and digests at their times: sets every task afresh as the term starts,
/// then sleeps until the next task is due, a trigger marks a change, or a
/// minute passes. Stops when the leader's term ends, after the pass in
/// progress, and gives how many tasks it ran.
fn spawn_tasks(db: store::Db, heartbeat: Heartbeat, term: Term) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let mut woken = crate::tasks::subscribe();
        let mut ran = 0;
        // Whatever a trigger might have missed, or a former leader left
        // half done, is set again from the data.
        if let Some(Err(err)) = with_db(&db, "tasks", logic::mark_everything).await {
            tracing::warn!("tasks: couldn't mark everything: {err}");
        }
        while !term.is_over() {
            // Instrumented, not entered: a guard held across the await below
            // stayed entered on the thread, and other tasks' spans joined this
            // trace until it ran past Tempo's size limit.
            let span = tracing::info_span!(parent: None, "tasks");
            let delay = async {
                let current = tracing::Span::current();
                let pass = with_db(&db, "tasks", move |conn| {
                    current.in_scope(|| logic::run_tasks(conn, Utc::now(), TASKS_PER_PASS))
                })
                .await;
                match pass {
                    Some(Ok(pass)) => {
                        heartbeat.beat();
                        ran += pass.ran;
                        for note in pass.report.notes {
                            tracing::info!("{note}");
                        }
                        // Saved with the tasks' changes; the job worker picks them up.
                        if !pass.report.jobs.is_empty() {
                            crate::jobs::wake();
                        }
                        for guild_id in pass.report.guilds {
                            crate::live::server(guild_id);
                        }
                        // A change may have set a Google sync.
                        if pass.marks > 0 {
                            crate::tasks::wake_google();
                        }
                        if pass.ran >= TASKS_PER_PASS {
                            std::time::Duration::ZERO
                        } else {
                            until(pass.next, TASKS_IDLE)
                        }
                    }
                    Some(Err(err)) => {
                        tracing::warn!("tasks failed: {err}");
                        TASKS_BACK_OFF
                    }
                    None => TASKS_BACK_OFF,
                }
            }
            .instrument(span)
            .await;
            if delay.is_zero() {
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = woken.changed() => {}
                _ = term.ended() => break,
            }
        }
        ran
    })
}

/// How long until `next`, at most `longest`; nothing when it's due.
fn until(next: Option<chrono::DateTime<Utc>>, longest: std::time::Duration) -> std::time::Duration {
    match next {
        Some(at) => (at - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO)
            .min(longest),
        None => longest,
    }
}

/// Google Calendar sync, apart from the task worker so a slow Google API
/// never holds up posts and reminders: one person at a time, as their
/// calendar's task comes due. Stops when the leader's term ends.
fn spawn_google_sync(
    db: store::Db,
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    public_base: String,
    term: Term,
) -> Option<tokio::task::JoinHandle<()>> {
    if !crate::google::configured(&client_id, &client_secret) {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut woken = crate::tasks::subscribe_google();
        while !term.is_over() {
            let claimed = with_db(&db, "google calendar", |conn| {
                logic::claim_google(conn, Utc::now())
            })
            .await;
            let delay = match claimed {
                Some(Ok(Some((user_id, lease)))) => {
                    let span = tracing::info_span!(parent: None, "google_sync");
                    let again = sync_google_user(
                        &db,
                        &http,
                        &client_id,
                        &client_secret,
                        &public_base,
                        user_id,
                    )
                    .instrument(span)
                    .await;
                    let finished = with_db(&db, "google calendar", move |conn| {
                        logic::finish_google(conn, user_id, lease, again, Utc::now())
                    })
                    .await;
                    if let Some(Err(err)) = finished {
                        tracing::warn!("google calendar: {err}");
                    }
                    continue;
                }
                Some(Ok(None)) => match with_db(&db, "google calendar", logic::next_google).await {
                    Some(Ok(next)) => until(next, TASKS_IDLE),
                    Some(Err(err)) => {
                        tracing::warn!("google calendar: {err}");
                        TASKS_BACK_OFF
                    }
                    None => TASKS_BACK_OFF,
                },
                Some(Err(err)) => {
                    tracing::warn!("google calendar: {err}");
                    TASKS_BACK_OFF
                }
                None => TASKS_BACK_OFF,
            };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = woken.changed() => {}
                _ = term.ended() => break,
            }
        }
    }))
}

/// Hourly, from the start of the leader's term: deletes sign-ins past their
/// 30 days, which hold Discord tokens nobody can use any more.
fn spawn_session_sweep(db: store::Db, term: Term) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(3600));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = every.tick() => {}
                _ = term.ended() => break,
            }
            match with_db(&db, "session sweep", |conn| {
                store::delete_expired_sessions(conn, Utc::now())
            })
            .await
            {
                Some(Ok(0)) | None => {}
                Some(Ok(gone)) => tracing::info!("Deleted {gone} expired sign-ins."),
                Some(Err(err)) => tracing::warn!("session sweep failed: {err}"),
            }
        }
    })
}

/// What only the leader runs: the Discord gateway and job worker, the task
/// worker, Google Calendar sync and the sign-in sweep. Starts them for
/// `term`, and once it ends stops them, letting the task in hand finish with
/// its jobs saved and, at shutdown, due Discord jobs drain.
async fn lead(state: AppState, token: String, status: String, term: Term) {
    // A fresh term gets the full grace period before `/healthz` minds the
    // task worker.
    state.heartbeat.beat();
    let gateway = crate::gateway::start(
        token,
        state.db.clone(),
        state.discord.clone(),
        state.public_url.clone(),
        status,
        state.members.clone(),
        term.clone(),
    );
    let tasks = spawn_tasks(state.db.clone(), state.heartbeat.clone(), term.clone());
    let sync = spawn_google_sync(
        state.db.clone(),
        state.http.clone(),
        state.google_client_id.clone(),
        state.google_client_secret.clone(),
        state.public_url.clone(),
        term.clone(),
    );
    let sweep = spawn_session_sweep(state.db.clone(), term.clone());
    term.ended().await;
    let finish = std::time::Duration::from_secs(10);
    if tokio::time::timeout(finish, tasks).await.is_err() {
        tracing::warn!("The task worker didn't finish in time.");
    }
    gateway.stop().await;
    for (what, task) in [
        ("Google Calendar sync", sync),
        ("The sign-in sweep", Some(sweep)),
    ] {
        let Some(mut task) = task else { continue };
        if tokio::time::timeout(finish, &mut task).await.is_err() {
            tracing::warn!("{what} didn't finish in time.");
            task.abort();
        }
    }
}

use crate::api::{cookie, Viewer as Session};
use crate::leader::Term;

/// Runs Marian: the web service with `pages` for everything else, on every
/// replica, and the gateway and the task worker on the one that leads.
pub fn run(pages: impl FnOnce(crate::api::Web) -> Router + Send + 'static) -> anyhow::Result<()> {
    store::load_dotenv();
    let telemetry = crate::observe::init();

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
        discord: tx,
        invite_url,
        http,
        client_id,
        client_secret,
        public_url,
        google_client_id,
        google_client_secret,
        bot_token: token.clone(),
        member_roles: member_roles_cache(),
        members: crate::gateway::MemberView::default(),
        heartbeat: Heartbeat::new(),
        leader: crate::leader::Status::default(),
    };
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async move {
        tokio::spawn(crate::shutdown::on_signal());
        let web = crate::api::Web {
            db: state.db.clone(),
            invite_url: state.invite_url.clone(),
            oauth_ready: oauth_ready(&state),
        };
        let app = router(state.clone(), pages(web));
        let saver = crate::gateway::Saver::start(state.db.clone(), rx);
        crate::live::bridge(database_url.clone());
        let leading = tokio::spawn(crate::leader::run(
            database_url,
            crate::leader::LEADER_KEY,
            state.leader.clone(),
            {
                let state = state.clone();
                move |term| lead(state.clone(), token.clone(), status.clone(), term)
            },
        ));

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Marian is listening on {addr}");
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(crate::shutdown::wait());
        let served = std::future::IntoFuture::into_future(served);
        tokio::pin!(served);
        // Requests still open this long after the signal are cut off.
        let cut_off = async {
            crate::shutdown::wait().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        };
        tokio::select! {
            served = &mut served => served?,
            _ = cut_off => tracing::warn!("Requests still open at shutdown were cut off."),
        }
        // The leader finishes its task and due Discord jobs, disconnects,
        // then lets the lock go for the next replica.
        if tokio::time::timeout(std::time::Duration::from_secs(45), leading)
            .await
            .is_err()
        {
            tracing::warn!("The leader's work didn't stop in time.");
        }
        saver.stop().await;
        tracing::info!("Marian stopped.");
        Ok::<_, anyhow::Error>(())
    });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    // Sends the last spans and metrics.
    drop(telemetry);
    result
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
        .layer(middleware::from_fn_with_state(state.clone(), require_login))
        // Outside the login check, so a preflight is answered without a session.
        .layer(cors(&state.public_url))
        .layer(middleware::from_fn(lock_down_non_pages))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(middleware::from_fn(mark_page_trace))
        .layer(http_trace())
}

/// Only Marian's own pages may call the API from a browser, with the
/// signed-in person's cookie: the origin of `MARIAN_PUBLIC_URL`. Without one
/// (local runs) no origin is named, and browsers keep to same-origin.
fn cors(public_url: &str) -> tower_http::cors::CorsLayer {
    use tower_http::cors::{AllowOrigin, CorsLayer};
    let origins: Vec<HeaderValue> = public_origin(public_url)
        .and_then(|origin| HeaderValue::from_str(&origin).ok())
        .into_iter()
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers([header::CONTENT_TYPE, header::ACCEPT])
        .max_age(std::time::Duration::from_secs(3600))
}

/// `https://marian.example.com` from `https://marian.example.com/some/path`.
fn public_origin(public_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(public_url.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

/// What isn't a page (JSON, calendar feeds, redirects, files) may load and
/// run nothing, nor be framed. Pages send their own policy from the app shell,
/// with the nonce their scripts carry.
const NON_PAGE_POLICY: &str =
    "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

async fn lock_down_non_pages(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let page = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|kind| kind.starts_with("text/html"));
    if !page
        && !response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(NON_PAGE_POLICY),
        );
    }
    response
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
            parent: None,
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

/// A file name with cargo-leptos's content hash in it, as `marian.<hash>.wasm`:
/// 22 characters of URL-safe base64.
fn hashed_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    name.split('.').any(|part| {
        part.len() == 22
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
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

/// Site files with their content hash in the name never change, so browsers
/// keep them for a year without asking. A build without hashed names keeps
/// fixed names, and a browser that guesses one is still fresh could pair the
/// script with a newer wasm and fail to start, so those are asked about each
/// time. A file that hasn't changed since the browser's copy is answered with
/// 304 here, in case whatever served it didn't look at `If-Modified-Since`.
async fn revalidate_site(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/pkg/") {
        return next.run(request).await;
    }
    if hashed_name(path) {
        let mut response = next.run(request).await;
        if response.status() == StatusCode::OK {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
        }
        return response;
    }
    let since = request.headers().get(header::IF_MODIFIED_SINCE).cloned();
    let mut response = next.run(request).await;
    let unchanged = response.status() == StatusCode::OK
        && since.is_some()
        && response.headers().get(header::LAST_MODIFIED) == since.as_ref();
    if unchanged {
        let mut fresh = StatusCode::NOT_MODIFIED.into_response();
        if let Some(modified) = response.headers().get(header::LAST_MODIFIED) {
            fresh
                .headers_mut()
                .insert(header::LAST_MODIFIED, modified.clone());
        }
        response = fresh;
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Liveness: on the leader, fails once the task worker hasn't finished a pass in two
/// minutes, so a stuck process gets restarted and another replica takes over.
/// A follower runs no tasks. The body says which this replica is. The
/// database's own health is `/readyz`.
async fn health(State(state): State<AppState>) -> Response {
    liveness(
        state.leader.leading(),
        &state.heartbeat,
        Utc::now().timestamp(),
    )
    .into_response()
}

fn liveness(leading: bool, heartbeat: &Heartbeat, now: i64) -> (StatusCode, &'static str) {
    match (leading, heartbeat.stale(now)) {
        (true, true) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "leader: the task worker has stalled",
        ),
        (true, false) => (StatusCode::OK, "ok: leader"),
        (false, _) => (StatusCode::OK, "ok: follower"),
    }
}

async fn ready(State(state): State<AppState>) -> Response {
    let ok = store::blocking(|| {
        state.db.get().map(|mut conn| {
            diesel::select(diesel::dsl::sql::<diesel::sql_types::Integer>("1"))
                .get_result::<i32>(&mut *conn)
                .is_ok()
        })
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
    if signed_in && recheck_session_guilds(&state, &headers).await {
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

    /// Whether this request should ask Discord: the first one in a window
    /// claims it at once, so the other requests a page fires alongside it, and
    /// any retry after a failure (a 429 above all), wait for the next window.
    fn claim(&mut self, guild: &str, user: &str, now: std::time::Instant) -> bool {
        if self.fresh(guild, user, now) {
            return false;
        }
        self.remember(guild, user, now);
        true
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
    if !state.member_roles.lock().expect("member roles").claim(
        &target.guild_discord,
        &target.user_discord,
        std::time::Instant::now(),
    ) {
        return;
    }
    // The bot's member list already has them while the gateway follows members.
    if let Some(roles) = state
        .members
        .roles(&target.guild_discord, &target.user_discord)
    {
        store_member_roles(state, &target, &roles);
        return;
    }
    let Some((status, body)) = discord_member(
        &state.http,
        &state.bot_token,
        &target.guild_discord,
        &target.user_discord,
    )
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
    let mut db = state.db.get().expect("database");
    role_target(&mut db, headers, server)
}

fn store_member_roles(state: &AppState, target: &RoleTarget, roles: &[String]) {
    let mut db = state.db.get().expect("database");
    if let Err(err) =
        store::replace_discord_roles(&mut db, target.guild_id, target.membership_id, roles)
    {
        tracing::warn!("could not store Discord roles: {err}");
    }
}

fn server_in_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/servers/")?;
    let id = rest.split('/').next()?;
    if snowflake(id) {
        Some(id)
    } else {
        None
    }
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
    let user = logic::admit_discord_user(
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
        Ok(member) => FreshRoles::Known(
            member
                .roles
                .into_iter()
                .filter(|id| snowflake(id))
                .collect(),
        ),
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
        .header(header::AUTHORIZATION, format!("Bot {}", bot_token.trim())),
    )
    .await
    .ok()?;
    let status = response.status();
    let body = response.text().await.ok()?;
    Some((status, body))
}

/// Makes the member at home in every Marian server they share on Discord and
/// returns those servers with their membership in each, oldest server first.
fn admit_guilds(
    db: &mut Connection,
    discord_user: &str,
    name: &str,
    member_of: &[MemberGuild],
) -> Result<Vec<(i64, i64)>, OpError> {
    let pairs: Vec<(i64, String)> = store::list_guilds(db)?
        .into_iter()
        .filter_map(|guild| Some((guild.id, guild.discord_id?)))
        .collect();
    let discord_ids: Vec<String> = member_of.iter().map(|guild| guild.id.clone()).collect();
    let mut admitted = Vec::new();
    for guild_id in auth::guilds_in_common(&pairs, &discord_ids) {
        let discord_id = pairs
            .iter()
            .find(|(id, _)| *id == guild_id)
            .map(|(_, discord_id)| discord_id.as_str())
            .unwrap_or("");
        let user = logic::admit_discord_user(db, guild_id, discord_user, name)?;
        let manages = member_of
            .iter()
            .find(|guild| guild.id == discord_id)
            .is_some_and(|guild| guild.manages);
        logic::set_server_manager(db, user.id, manages)?;
        admitted.push((guild_id, user.id));
    }
    Ok(admitted)
}

const SESSION_GUILDS_TTL: chrono::Duration = chrono::Duration::minutes(5);

/// Re-reads the member's Discord servers every few minutes, so a server Marian
/// joins after they sign in shows up, and one they leave drops out. Answers
/// whether the session is still good.
async fn recheck_session_guilds(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(token) = cookie(headers, "marian_session") else {
        return false;
    };
    let session = {
        let mut db = state.db.get().expect("database");
        match store::web_session(&mut db, &token) {
            Ok(Some(session)) => session,
            _ => return true,
        }
    };
    let Some(grant) = session.grant else {
        return true;
    };
    if Utc::now() - session.guilds_checked_at < SESSION_GUILDS_TTL {
        return true;
    }
    {
        let mut db = state.db.get().expect("database");
        if let Err(err) = store::touch_session_guilds(&mut db, &token) {
            tracing::warn!("could not note the server check: {err}");
            return true;
        }
    }
    let grant = if grant.expires_at - chrono::Duration::minutes(1) > Utc::now() {
        grant
    } else {
        let fresh = discord_refresh(state, &grant.refresh).await;
        let mut db = state.db.get().expect("database");
        match fresh {
            Ok(fresh) => {
                if let Err(err) = store::set_session_grant(&mut db, &token, Some(&fresh)) {
                    tracing::warn!("could not keep the refreshed Discord grant: {err}");
                }
                fresh
            }
            Err(err) => {
                if err.revoked() {
                    let _ = store::set_session_grant(&mut db, &token, None);
                }
                return true;
            }
        }
    };
    let member_of = match fetch_guilds(state, &grant.access).await {
        Ok(member_of) => member_of,
        Err(err) => {
            if err.revoked() {
                let mut db = state.db.get().expect("database");
                let _ = store::set_session_grant(&mut db, &token, None);
            }
            return true;
        }
    };
    let mut db = state.db.get().expect("database");
    let admitted = match admit_guilds(
        &mut db,
        &session.discord_user_id,
        &session.display_name,
        &member_of,
    ) {
        Ok(admitted) => admitted,
        Err(err) => {
            tracing::warn!("could not recheck the member's servers: {err}");
            return true;
        }
    };
    let Some(&home) = admitted.first() else {
        let _ = store::delete_web_session(&mut db, &token);
        return false;
    };
    let guilds: Vec<i64> = admitted.iter().map(|(guild_id, _)| *guild_id).collect();
    if let Err(err) = store::replace_session_guilds(&mut db, &token, &guilds, home) {
        tracing::warn!("could not store the member's servers: {err}");
    }
    true
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
    let grant = match discord_token(&state, code, &redirect_uri).await {
        Ok(grant) => grant,
        Err(err) => return nav_redirect("/", &err),
    };
    let access = grant.access.clone();
    let me = match discord_me(&state, &access).await {
        Ok(me) => me,
        Err(err) => return nav_redirect("/", &err),
    };
    let member_of = match discord_guilds(&state, &access).await {
        Ok(ids) => ids,
        Err(err) => return nav_redirect("/", &err),
    };
    let name = auth::discord_display_name(None, me.global_name.as_deref(), &me.username);
    let mut db = state.db.get().expect("database");
    let admitted = match admit_guilds(&mut db, &me.id, &name, &member_of) {
        Ok(admitted) => admitted,
        Err(err) => return plain_error(err),
    };
    let Some(&(guild_id, membership_id)) = admitted.first() else {
        return nav_redirect("/", "Marian isn't in a server you belong to yet.");
    };
    let allowed: Vec<i64> = admitted.iter().map(|(guild_id, _)| *guild_id).collect();
    let token = match auth::random_token() {
        Ok(token) => token,
        Err(err) => return plain_error(err),
    };
    if let Err(err) = store::insert_web_session(
        &mut db,
        &token,
        &me.id,
        &name,
        guild_id,
        membership_id,
        &allowed,
        Some(&grant),
    ) {
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

/// Brings one person's Google Calendar up to date. Returns when to look
/// again sooner than the full pass, if at all.
async fn sync_google_user(
    db: &store::Db,
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    public_base: &str,
    user_id: i64,
) -> Option<chrono::DateTime<Utc>> {
    // A round that couldn't finish is tried again soon.
    let soon = || Some(Utc::now() + chrono::Duration::minutes(5));
    let base = public_base.to_string();
    let (jobs, mut again) = match with_db(db, "google calendar", move |conn| {
        crate::google::plan_user(conn, &base, user_id)
    })
    .await
    {
        Some(Ok(planned)) => planned,
        Some(Err(err)) => {
            tracing::warn!("google calendar: {err}");
            return soon();
        }
        None => return soon(),
    };
    for job in jobs {
        let outcome = crate::google::push(http, client_id, client_secret, job).await;
        if let crate::google::Outcome::Failed {
            event_id, message, ..
        } = &outcome
        {
            tracing::warn!("google calendar event {event_id}: {message}");
        }
        match with_db(db, "google calendar", move |conn| {
            crate::google::record(conn, outcome)
        })
        .await
        {
            Some(Err(err)) => tracing::warn!("google calendar: {err}"),
            Some(Ok(_)) => {}
            // Without a database the rest of the round can't be recorded either.
            None => return soon(),
        }
    }
    let revokes = match with_db(db, "google calendar", move |conn| {
        crate::google::ready_revokes(conn, Some(user_id))
    })
    .await
    {
        Some(Ok(rows)) => rows,
        Some(Err(err)) => {
            tracing::warn!("google calendar: {err}");
            return soon();
        }
        None => return soon(),
    };
    for access in revokes {
        let user_id = access.user_id;
        if crate::google::forget_remote(http, client_id, client_secret, access).await {
            match with_db(db, "google calendar", move |conn| {
                crate::google::delete_account(conn, user_id)
            })
            .await
            {
                Some(Err(err)) => tracing::warn!("google calendar: {err}"),
                Some(Ok(_)) => {}
                None => return soon(),
            }
        } else {
            tracing::warn!("google calendar disconnect for {user_id} will retry");
            again = match (again, soon()) {
                (Some(at), Some(retry)) => Some(at.min(retry)),
                (at, retry) => at.or(retry),
            };
        }
    }
    again
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

async fn discord_token(
    state: &AppState,
    code: &str,
    redirect_uri: &str,
) -> Result<store::DiscordGrant, String> {
    discord_grant(
        state,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ],
    )
    .await
    .map_err(|err| err.message)
}

async fn discord_refresh(
    state: &AppState,
    refresh: &str,
) -> Result<store::DiscordGrant, DiscordError> {
    discord_grant(
        state,
        &[("grant_type", "refresh_token"), ("refresh_token", refresh)],
    )
    .await
}

async fn discord_grant(
    state: &AppState,
    form: &[(&str, &str)],
) -> Result<store::DiscordGrant, DiscordError> {
    let mut fields = vec![
        ("client_id", state.client_id.as_str()),
        ("client_secret", state.client_secret.as_str()),
    ];
    fields.extend_from_slice(form);
    let response = crate::observe::send_traced(
        state
            .http
            .post("https://discord.com/api/oauth2/token")
            .form(&fields),
    )
    .await
    .map_err(|_| DiscordError::new(None, "Discord didn't answer the sign-in."))?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!("discord token exchange returned {status}");
        return Err(DiscordError::new(
            Some(status),
            "Discord didn't accept the sign-in. Try again.",
        ));
    }
    let body: DiscordToken = response
        .json()
        .await
        .map_err(|_| DiscordError::new(None, "Discord sent an unexpected sign-in reply."))?;
    if body.access_token.is_empty() {
        return Err(DiscordError::new(
            None,
            "Discord sent an unexpected sign-in reply.",
        ));
    }
    Ok(store::DiscordGrant {
        access: body.access_token,
        refresh: body.refresh_token,
        expires_at: Utc::now() + chrono::Duration::seconds(body.expires_in),
    })
}

struct DiscordError {
    status: Option<StatusCode>,
    message: String,
}

impl DiscordError {
    fn new(status: Option<StatusCode>, message: &str) -> Self {
        Self {
            status,
            message: message.to_string(),
        }
    }

    /// Discord no longer honours the grant, so asking again won't help.
    fn revoked(&self) -> bool {
        matches!(
            self.status,
            Some(StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED)
        )
    }
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
    fetch_guilds(state, access).await.map_err(|err| err.message)
}

async fn fetch_guilds(state: &AppState, access: &str) -> Result<Vec<MemberGuild>, DiscordError> {
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
            .map_err(|_| DiscordError::new(None, "Discord didn't answer with your servers."))?;
        let status = response.status();
        if !status.is_success() {
            tracing::warn!("discord guilds returned {status}");
            return Err(DiscordError::new(
                Some(status),
                "Discord didn't share your servers. Try again.",
            ));
        }
        let body = response
            .text()
            .await
            .map_err(|_| DiscordError::new(None, "Discord sent an unexpected server list."))?;
        let guilds = member_guilds(&body).map_err(|err| {
            tracing::warn!("discord guild list did not match: {err}");
            DiscordError::new(None, "Discord sent an unexpected server list.")
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
    #[serde(default)]
    refresh_token: String,
    #[serde(default = "week_seconds")]
    expires_in: i64,
}

fn week_seconds() -> i64 {
    7 * 24 * 3600
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
    // Calendar apps poll this, so it stays off the async workers.
    store::blocking(|| {
        let Ok(mut db) = state.db.get() else {
            return (StatusCode::SERVICE_UNAVAILABLE, "The database is busy.").into_response();
        };
        let Some(user_id) = store::calendar_user_id(&mut db, token).ok().flatten() else {
            return (StatusCode::NOT_FOUND, "No calendar").into_response();
        };
        match ics::user_feed(&mut db, user_id) {
            Ok(body) => calendar_response(body),
            Err(err) => plain_error(err),
        }
    })
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

/// An error for a sign-in or Google page. Database detail stays in the log.
fn plain_error(err: OpError) -> Response {
    let status = match &err {
        OpError::NotFound => StatusCode::NOT_FOUND,
        OpError::Message(_) | OpError::Text(_) => StatusCode::UNPROCESSABLE_ENTITY,
        OpError::Db(_) | OpError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_json(status, &err.localized(crate::i18n::Lang::En))
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

    #[tokio::test]
    async fn a_sign_in_page_shows_no_database_detail() {
        let err = OpError::Db(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            Box::new(
                r#"duplicate key value violates unique constraint "google_accounts_pkey""#
                    .to_string(),
            ),
        ));
        let response = plain_error(err);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body.contains("google_accounts_pkey"), "{body}");
        assert!(body.contains("Something went wrong"), "{body}");
    }

    /// Inserts a scheduled event of `id` due to post now, and wakes the worker.
    fn post_due(conn: &mut Connection, id: i64) {
        diesel::RunQueryDsl::execute(
            diesel::sql_query(format!(
                "INSERT INTO events (id, public_id, guild_id, channel_id, title, description, starts_at, \
                 duration_minutes, status, post_at, created_at, updated_at) VALUES \
                 ({id}, gen_random_uuid(), 1, 1, 'Raid {id}', '', now() + interval '1 day', 60, \
                 'scheduled', now() - interval '1 minute', now(), now())"
            )),
            conn,
        )
        .unwrap();
        crate::tasks::wake();
    }

    fn posted(conn: &mut Connection, id: i64) -> bool {
        schema::events::table
            .find(id)
            .select(schema::events::status)
            .first::<crate::values::EventStatus>(conn)
            .unwrap()
            == crate::values::EventStatus::Posted
    }

    async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..300 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("timed out waiting until {what}");
    }

    /// Ends the term of whoever holds `key`, as a lost database connection would.
    fn cut_lock(admin: &mut Connection, key: i64) {
        #[derive(QueryableByName)]
        struct Holder {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            pid: i32,
        }
        let holder: Holder = diesel::sql_query(format!(
            "SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND granted AND objsubid = 1 \
             AND ((classid::bigint << 32) | objid::bigint) = {key}"
        ))
        .get_result(admin)
        .unwrap();
        diesel::sql_query(format!("SELECT pg_terminate_backend({})", holder.pid))
            .execute(admin)
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn only_the_leader_runs_tasks_and_the_next_leader_takes_them_over() {
        use diesel::connection::{Connection as _, SimpleConnection};
        use std::sync::atomic::AtomicUsize;
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "INSERT INTO guilds (id, name, timezone) VALUES (1, 'Test', 'UTC');
             INSERT INTO settings (guild_id) VALUES (1);
             INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);",
        )
        .unwrap();
        let db = store::pool_for(&mut conn).unwrap();
        let url = store::database_url().unwrap();
        // A key of its own, so test runs and a local Marian don't meet.
        let key = 0x7461_736b_0000_0000 | i64::from(uuid::Uuid::now_v7().as_u128() as u32);
        // Tasks each node's worker ran, counted as its term ends.
        let ran = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
        let node = |index: usize| {
            let status = crate::leader::Status::default();
            let (db, ran) = (db.clone(), ran[index].clone());
            let task = tokio::spawn(crate::leader::run(
                url.clone(),
                key,
                status.clone(),
                move |term| {
                    let (db, ran) = (db.clone(), ran.clone());
                    async move {
                        let count = spawn_tasks(db, Heartbeat::new(), term).await.unwrap_or(0);
                        ran.fetch_add(count, Ordering::SeqCst);
                    }
                },
            ));
            (status, task)
        };
        let (first, first_task) = node(0);
        eventually("the first node leads", || first.leading()).await;
        let (second, second_task) = node(1);
        post_due(&mut conn, 1);
        eventually("the leader posts", || posted(&mut conn, 1)).await;
        post_due(&mut conn, 2);
        eventually("the leader posts again", || posted(&mut conn, 2)).await;
        assert!(first.leading() && !second.leading());

        let mut admin = Connection::establish(&url).unwrap();
        cut_lock(&mut admin, key);
        eventually("the first node steps down", || !first.leading()).await;
        eventually("its worker stops", || ran[0].load(Ordering::SeqCst) > 0).await;
        assert_eq!(
            ran[0].load(Ordering::SeqCst),
            2,
            "the leader ran both posts"
        );
        assert_eq!(ran[1].load(Ordering::SeqCst), 0, "a follower runs nothing");
        eventually("the second node takes over", || second.leading()).await;
        post_due(&mut conn, 3);
        eventually("the new leader posts", || posted(&mut conn, 3)).await;
        cut_lock(&mut admin, key);
        eventually("its worker stops", || ran[1].load(Ordering::SeqCst) > 0).await;
        assert_eq!(
            ran[1].load(Ordering::SeqCst),
            1,
            "only the post left for it"
        );

        let jobs: i64 = schema::discord_jobs::table
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(jobs, 6, "each post's sync and attend, once");
        first_task.abort();
        second_task.abort();
    }

    #[test]
    fn health_fails_once_the_task_worker_stalls() {
        let heartbeat = Heartbeat::new();
        let now = Utc::now().timestamp();
        assert!(!heartbeat.stale(now));
        assert!(!heartbeat.stale(now + TASKS_STALE_AFTER_SECS));
        assert!(heartbeat.stale(now + TASKS_STALE_AFTER_SECS + 5));
        heartbeat.0.store(now - 600, Ordering::Relaxed);
        assert!(heartbeat.stale(now));
        heartbeat.beat();
        assert!(!heartbeat.stale(now));
    }

    #[test]
    fn only_the_leader_minds_the_task_worker() {
        let heartbeat = Heartbeat::new();
        let later = Utc::now().timestamp() + TASKS_STALE_AFTER_SECS + 5;
        assert_eq!(
            liveness(true, &heartbeat, later).0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            liveness(false, &heartbeat, later),
            (StatusCode::OK, "ok: follower")
        );
        heartbeat.beat();
        let now = Utc::now().timestamp();
        assert_eq!(
            liveness(true, &heartbeat, now),
            (StatusCode::OK, "ok: leader")
        );
    }

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
    fn only_the_first_request_in_a_window_asks_discord() {
        let now = std::time::Instant::now();
        let mut roles = MemberRoles::default();
        assert!(roles.claim("99", "42", now), "the first request asks");
        assert!(
            !roles.claim("99", "42", now),
            "a request fired alongside it waits"
        );
        assert!(
            !roles.claim("99", "42", now + std::time::Duration::from_secs(29)),
            "so does a retry"
        );
        assert!(
            roles.claim("99", "42", now + MEMBER_ROLES_TTL),
            "the next window asks again"
        );
        assert!(roles.claim("7", "42", now), "each server is its own lookup");
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

    #[test]
    fn hashed_site_files_are_told_apart() {
        assert!(hashed_name("/pkg/marian.iucsWZX9RG8HB2_vQoyuAw.wasm"));
        assert!(hashed_name("/pkg/marian.p8RBG6uYGacG1M0EJS3XOg.js"));
        assert!(hashed_name("/pkg/marian.kcWcuR9ygyaOXPYiW0cpOw.css"));
        assert!(!hashed_name("/pkg/marian.wasm"));
        assert!(!hashed_name("/pkg/marian_bg.wasm"));
    }

    #[test]
    fn the_public_url_gives_its_origin() {
        assert_eq!(
            public_origin("https://marian.example.com/app/").as_deref(),
            Some("https://marian.example.com")
        );
        assert_eq!(
            public_origin(" http://localhost:8080 ").as_deref(),
            Some("http://localhost:8080")
        );
        assert_eq!(public_origin(""), None);
        assert_eq!(public_origin("ftp://marian.example.com"), None);
    }

    #[tokio::test]
    async fn only_marian_itself_may_call_the_api_and_non_pages_run_nothing() {
        let mut conn = store::open_memory().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState {
            db: store::pool_for(&mut conn).unwrap(),
            discord: tx,
            invite_url: String::new(),
            http: reqwest::Client::new(),
            client_id: String::new(),
            client_secret: String::new(),
            public_url: "https://marian.example.com/".into(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            bot_token: String::new(),
            member_roles: member_roles_cache(),
            members: crate::gateway::MemberView::default(),
            heartbeat: Heartbeat::new(),
            leader: crate::leader::Status::default(),
        };
        let pages = Router::new().route(
            "/page",
            get(|| async {
                (
                    [(header::CONTENT_SECURITY_POLICY, "from the shell")],
                    axum::response::Html("<p>hi</p>"),
                )
            }),
        );
        let app = router(state, pages);
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
        let client = reqwest::Client::new();
        let csp = |response: &reqwest::Response| {
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .map(|v| v.to_str().unwrap().to_string())
        };
        let allowed = |response: &reqwest::Response| {
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().unwrap().to_string())
        };

        let health = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(csp(&health).as_deref(), Some(NON_PAGE_POLICY));
        let refused = client
            .get(format!("http://{addr}/api/servers/1/events"))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            csp(&refused).as_deref(),
            Some(NON_PAGE_POLICY),
            "JSON errors too"
        );
        let page = client
            .get(format!("http://{addr}/page"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            csp(&page).as_deref(),
            Some("from the shell"),
            "a page keeps its own policy"
        );

        let own = client
            .get(format!("http://{addr}/healthz"))
            .header(header::ORIGIN, "https://marian.example.com")
            .send()
            .await
            .unwrap();
        assert_eq!(allowed(&own).as_deref(), Some("https://marian.example.com"));
        assert_eq!(
            own.headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .unwrap(),
            "true"
        );
        let foreign = client
            .get(format!("http://{addr}/healthz"))
            .header(header::ORIGIN, "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(allowed(&foreign), None);

        let preflight = client
            .request(
                axum::http::Method::OPTIONS,
                format!("http://{addr}/api/servers/1/events"),
            )
            .header(header::ORIGIN, "https://marian.example.com")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
            .send()
            .await
            .unwrap();
        assert!(
            preflight.status().is_success(),
            "answered before the login check: {}",
            preflight.status()
        );
        assert_eq!(
            allowed(&preflight).as_deref(),
            Some("https://marian.example.com")
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
            members: crate::gateway::MemberView::default(),
            heartbeat: Heartbeat::new(),
            leader: crate::leader::Status::default(),
        };
        let app = router(state, Router::new().nest_service("/pkg", site_files(&dir)));
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
        // No automatic decoding, so the bytes on the wire are what's checked.
        let client = reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .build()
            .unwrap();
        let url = format!("http://{addr}/pkg/marian.wasm");
        let fetch = |encoding: &'static str| {
            let request = client.get(&url).header(header::ACCEPT_ENCODING, encoding);
            async move { request.send().await.unwrap() }
        };

        let br = fetch("gzip, br").await;
        assert_eq!(br.headers().get(header::CONTENT_ENCODING).unwrap(), "br");
        assert_eq!(br.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        let modified = br.headers().get(header::LAST_MODIFIED).cloned().unwrap();
        assert_eq!(
            br.bytes().await.unwrap().as_ref(),
            b"brotli bytes",
            "the stored copy, not compressed again"
        );

        // No .gz copy here: the plain file is compressed on the way instead.
        let gzip = fetch("gzip").await;
        assert_eq!(
            gzip.headers().get(header::CONTENT_ENCODING).unwrap(),
            "gzip"
        );
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

        // A file with its content hash in the name is kept for good, compressed copy and all.
        std::fs::write(dir.join("marian.iucsWZX9RG8HB2_vQoyuAw.wasm"), &plain).unwrap();
        std::fs::write(
            dir.join("marian.iucsWZX9RG8HB2_vQoyuAw.wasm.br"),
            b"brotli bytes",
        )
        .unwrap();
        let hashed = client
            .get(format!(
                "http://{addr}/pkg/marian.iucsWZX9RG8HB2_vQoyuAw.wasm"
            ))
            .header(header::ACCEPT_ENCODING, "br")
            .send()
            .await
            .unwrap();
        assert_eq!(
            hashed.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            hashed.headers().get(header::CONTENT_ENCODING).unwrap(),
            "br"
        );
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
            None,
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
            members: crate::gateway::MemberView::default(),
            heartbeat: Heartbeat::new(),
            leader: crate::leader::Status::default(),
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
            members: crate::gateway::MemberView::default(),
            heartbeat: Heartbeat::new(),
            leader: crate::leader::Status::default(),
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
