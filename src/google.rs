//! A person's Google Calendar. OAuth lets Marian create and update events there.

use std::collections::HashSet;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use serde_json::json;

use crate::auth;
use crate::clock;
use crate::error::OpError;
use crate::ics;
use crate::schema;
use crate::store::{self, Connection, Event};

const SCOPE: &str = "https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/userinfo.email";
const BATCH: usize = 4;

pub struct Status {
    pub email: String,
    pub connected: bool,
    pub reconnect: bool,
}

pub enum Job {
    Upsert {
        discord_user_id: String,
        event_id: i64,
        google_event_id: Option<String>,
        refresh_token: String,
        marian_id: String,
        snapshot: String,
        body: serde_json::Value,
    },
    Delete {
        discord_user_id: String,
        event_id: i64,
        google_event_id: String,
        refresh_token: String,
    },
}

pub enum Outcome {
    Saved {
        discord_user_id: String,
        event_id: i64,
        google_event_id: String,
        snapshot: String,
    },
    Removed {
        discord_user_id: String,
        event_id: i64,
    },
    Failed {
        discord_user_id: String,
        event_id: i64,
        message: String,
        auth_dead: bool,
    },
}

pub struct RemoteAccess {
    pub discord_user_id: String,
    pub refresh_token: String,
    pub google_event_ids: Vec<String>,
}

struct Grant {
    access_token: String,
    refresh_token: Option<String>,
}

enum GrantError {
    Dead(String),
    Retry(String),
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = schema::google_accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Account {
    discord_user_id: String,
    refresh_token: String,
    email: String,
    #[allow(dead_code)]
    connected_at: String,
    last_error: String,
    auto_sync: bool,
    revoking: bool,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = schema::google_events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct Link {
    discord_user_id: String,
    event_id: i64,
    google_event_id: Option<String>,
    content_hash: String,
    #[allow(dead_code)]
    last_error: String,
    retry_at: Option<String>,
    forced: bool,
}

pub fn configured(client_id: &str, client_secret: &str) -> bool {
    !client_id.trim().is_empty() && !client_secret.trim().is_empty()
}

pub fn configured_from_env() -> bool {
    configured(
        &std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
        &std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
    )
}

pub fn redirect_uri(public_url: &str, host: Option<&str>, proto: Option<&str>) -> Option<String> {
    let base = if !public_url.trim().is_empty() {
        public_url.trim().trim_end_matches('/').to_string()
    } else {
        let host = host?.trim();
        if host.is_empty() || host.contains('/') || host.contains(' ') {
            return None;
        }
        let proto = if proto == Some("https") {
            "https"
        } else {
            "http"
        };
        format!("{proto}://{host}")
    };
    Some(format!("{base}/auth/google/callback"))
}

pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        auth::encode(client_id.trim()),
        auth::encode(redirect_uri),
        auth::encode(SCOPE),
        auth::encode(state),
    )
}

pub fn status(conn: &mut Connection, discord_user_id: &str) -> Result<Option<Status>, OpError> {
    let account = account(conn, discord_user_id)?;
    Ok(account.map(|account| Status {
        connected: account.last_error.is_empty(),
        reconnect: !account.last_error.is_empty(),
        email: account.email,
    }))
}

pub fn on_calendar(
    conn: &mut Connection,
    discord_user_id: &str,
    event_id: i64,
) -> Result<bool, OpError> {
    let id: Option<Option<String>> = schema::google_events::table
        .filter(schema::google_events::discord_user_id.eq(discord_user_id))
        .filter(schema::google_events::event_id.eq(event_id))
        .select(schema::google_events::google_event_id)
        .first(conn)
        .optional()?;
    Ok(id.flatten().is_some())
}

pub fn save_account(
    conn: &mut Connection,
    discord_user_id: &str,
    refresh_token: &str,
    email: &str,
) -> Result<(), OpError> {
    let now = store::now_stamp();
    diesel::insert_into(schema::google_accounts::table)
        .values((
            schema::google_accounts::discord_user_id.eq(discord_user_id),
            schema::google_accounts::refresh_token.eq(refresh_token),
            schema::google_accounts::email.eq(email),
            schema::google_accounts::connected_at.eq(&now),
            schema::google_accounts::last_error.eq(""),
        ))
        .on_conflict(schema::google_accounts::discord_user_id)
        .do_update()
        .set((
            schema::google_accounts::refresh_token.eq(refresh_token),
            schema::google_accounts::email.eq(email),
            schema::google_accounts::connected_at.eq(&now),
            schema::google_accounts::last_error.eq(""),
            schema::google_accounts::auto_sync.eq(true),
            schema::google_accounts::revoking.eq(false),
        ))
        .execute(conn)?;
    Ok(())
}

pub fn begin_link(conn: &mut Connection, discord_user_id: &str) -> Result<String, OpError> {
    diesel::delete(
        schema::google_oauth::table
            .filter(schema::google_oauth::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    let state = auth::random_token()?;
    diesel::insert_into(schema::google_oauth::table)
        .values((
            schema::google_oauth::state.eq(&state),
            schema::google_oauth::discord_user_id.eq(discord_user_id),
            schema::google_oauth::created_at.eq(store::now_stamp()),
        ))
        .execute(conn)?;
    Ok(state)
}

pub fn connect_url(public_base: &str, state: &str) -> Option<String> {
    let base = public_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/auth/google/start/{state}"))
}

pub fn claim_oauth(conn: &mut Connection, state: &str) -> Result<Option<String>, OpError> {
    let row: Option<(String, String)> = schema::google_oauth::table
        .filter(schema::google_oauth::state.eq(state))
        .select((
            schema::google_oauth::discord_user_id,
            schema::google_oauth::created_at,
        ))
        .first(conn)
        .optional()?;
    let Some((discord_user_id, created_at)) = row else {
        return Ok(None);
    };
    let fresh = clock::parse_stamp(&created_at)
        .ok()
        .is_some_and(|at| Utc::now() - at < Duration::minutes(10));
    if !fresh {
        finish_oauth(conn, state)?;
        return Ok(None);
    }
    Ok(Some(discord_user_id))
}

pub fn finish_oauth(conn: &mut Connection, state: &str) -> Result<(), OpError> {
    diesel::delete(schema::google_oauth::table.filter(schema::google_oauth::state.eq(state)))
        .execute(conn)?;
    Ok(())
}

pub fn set_auto_sync(
    conn: &mut Connection,
    discord_user_id: &str,
    on: bool,
) -> Result<(), OpError> {
    let Some(account) = account(conn, discord_user_id)? else {
        return Err(OpError::message(
            "Connect Google Calendar with /google authorize.",
        ));
    };
    if account.revoking {
        return Err(OpError::message(
            "Google Calendar is disconnecting. /google authorize connects it again.",
        ));
    }
    diesel::update(
        schema::google_accounts::table
            .filter(schema::google_accounts::discord_user_id.eq(discord_user_id)),
    )
    .set(schema::google_accounts::auto_sync.eq(on))
    .execute(conn)?;
    Ok(())
}

pub fn queue_revoke(conn: &mut Connection, discord_user_id: &str) -> Result<bool, OpError> {
    let updated = diesel::update(
        schema::google_accounts::table
            .filter(schema::google_accounts::discord_user_id.eq(discord_user_id)),
    )
    .set(schema::google_accounts::revoking.eq(true))
    .execute(conn)?;
    Ok(updated > 0)
}

pub fn queue_event(
    conn: &mut Connection,
    discord_user_id: &str,
    event_id: i64,
) -> Result<(), OpError> {
    let Some(account) = account(conn, discord_user_id)? else {
        return Err(OpError::message(
            "Connect Google Calendar with /google authorize.",
        ));
    };
    if account.revoking {
        return Err(OpError::message(
            "Google Calendar is disconnecting. /google authorize connects it again.",
        ));
    }
    diesel::insert_into(schema::google_events::table)
        .values((
            schema::google_events::discord_user_id.eq(discord_user_id),
            schema::google_events::event_id.eq(event_id),
            schema::google_events::content_hash.eq(""),
            schema::google_events::last_error.eq(""),
            schema::google_events::retry_at.eq(Option::<String>::None),
            schema::google_events::forced.eq(true),
        ))
        .on_conflict((
            schema::google_events::discord_user_id,
            schema::google_events::event_id,
        ))
        .do_update()
        .set((
            schema::google_events::content_hash.eq(""),
            schema::google_events::last_error.eq(""),
            schema::google_events::retry_at.eq(Option::<String>::None),
            schema::google_events::forced.eq(true),
        ))
        .execute(conn)?;
    Ok(())
}

pub fn ready_revokes(conn: &mut Connection) -> Result<Vec<RemoteAccess>, OpError> {
    let accounts = schema::google_accounts::table
        .filter(schema::google_accounts::revoking.eq(true))
        .select(Account::as_select())
        .load(conn)?;
    let mut ready = Vec::new();
    for account in accounts {
        let pending: i64 = schema::google_events::table
            .filter(schema::google_events::discord_user_id.eq(&account.discord_user_id))
            .filter(schema::google_events::google_event_id.is_not_null())
            .count()
            .get_result(conn)?;
        if pending > 0 {
            continue;
        }
        diesel::delete(
            schema::google_events::table
                .filter(schema::google_events::discord_user_id.eq(&account.discord_user_id)),
        )
        .execute(conn)?;
        ready.push(RemoteAccess {
            discord_user_id: account.discord_user_id,
            refresh_token: account.refresh_token,
            google_event_ids: Vec::new(),
        });
    }
    Ok(ready)
}

pub fn remote_access(
    conn: &mut Connection,
    discord_user_id: &str,
) -> Result<Option<RemoteAccess>, OpError> {
    let Some(account) = account(conn, discord_user_id)? else {
        return Ok(None);
    };
    let ids = schema::google_events::table
        .filter(schema::google_events::discord_user_id.eq(discord_user_id))
        .select(schema::google_events::google_event_id)
        .load::<Option<String>>(conn)?;
    Ok(Some(RemoteAccess {
        discord_user_id: account.discord_user_id,
        refresh_token: account.refresh_token,
        google_event_ids: ids.into_iter().flatten().collect(),
    }))
}

pub fn delete_account(conn: &mut Connection, discord_user_id: &str) -> Result<(), OpError> {
    diesel::delete(
        schema::google_events::table
            .filter(schema::google_events::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    diesel::delete(
        schema::google_accounts::table
            .filter(schema::google_accounts::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    Ok(())
}

pub fn plan(conn: &mut Connection, public_base: &str) -> Result<Vec<Job>, OpError> {
    let now = Utc::now();
    let accounts = schema::google_accounts::table
        .select(Account::as_select())
        .load(conn)?;
    let mut jobs = Vec::new();
    for account in accounts {
        if jobs.len() >= BATCH {
            break;
        }
        let links = links_for(conn, &account.discord_user_id)?;
        if account.revoking {
            queue_deletes(conn, &account, &links, now, &mut jobs)?;
            continue;
        }
        if !account.last_error.is_empty() {
            continue;
        }
        let desired = if account.auto_sync {
            ics::events_for_person(conn, &account.discord_user_id)?
        } else {
            Vec::new()
        };
        let wanted: HashSet<i64> = desired.iter().map(|event| event.id).collect();
        for event in &desired {
            if jobs.len() >= BATCH {
                break;
            }
            consider_upsert(conn, &account, event, &links, public_base, now, &mut jobs)?;
        }
        if account.auto_sync {
            queue_stale(conn, &account, &links, &wanted, now, &mut jobs)?;
        }
        for link in &links {
            if jobs.len() >= BATCH || !link.forced || waiting(link, now) {
                continue;
            }
            if jobs.iter().any(|job| job_event(job) == Some(link.event_id)) {
                continue;
            }
            let Some(event) = store::event(conn, link.event_id)? else {
                drop_link(conn, link)?;
                continue;
            };
            if !calendar_status(&event.status) {
                drop_link(conn, link)?;
                continue;
            }
            consider_upsert(conn, &account, &event, &links, public_base, now, &mut jobs)?;
        }
    }
    Ok(jobs)
}

fn calendar_status(status: &str) -> bool {
    matches!(status, "posted" | "scheduled" | "archived")
}

fn job_event(job: &Job) -> Option<i64> {
    match job {
        Job::Upsert { event_id, .. } | Job::Delete { event_id, .. } => Some(*event_id),
    }
}

fn consider_upsert(
    conn: &mut Connection,
    account: &Account,
    event: &Event,
    links: &[Link],
    public_base: &str,
    now: DateTime<Utc>,
    jobs: &mut Vec<Job>,
) -> Result<(), OpError> {
    let link = links.iter().find(|link| link.event_id == event.id);
    if link.is_some_and(|link| waiting(link, now)) {
        return Ok(());
    }
    let payload = payload(conn, event, public_base)?;
    let same = link.is_some_and(|link| {
        link.google_event_id.is_some() && link.content_hash == payload.snapshot
    });
    if same {
        if link.is_some_and(|link| link.forced) {
            clear_forced(conn, &account.discord_user_id, event.id)?;
        }
        return Ok(());
    }
    jobs.push(Job::Upsert {
        discord_user_id: account.discord_user_id.clone(),
        event_id: event.id,
        google_event_id: link.and_then(|link| link.google_event_id.clone()),
        refresh_token: account.refresh_token.clone(),
        marian_id: event.public_id.clone(),
        snapshot: payload.snapshot,
        body: payload.body,
    });
    Ok(())
}

fn queue_stale(
    conn: &mut Connection,
    account: &Account,
    links: &[Link],
    wanted: &HashSet<i64>,
    now: DateTime<Utc>,
    jobs: &mut Vec<Job>,
) -> Result<(), OpError> {
    for link in links {
        if jobs.len() >= BATCH || wanted.contains(&link.event_id) || waiting(link, now) {
            continue;
        }
        if link.forced {
            continue;
        }
        drop_or_delete(conn, account, link, jobs)?;
    }
    Ok(())
}

fn queue_deletes(
    conn: &mut Connection,
    account: &Account,
    links: &[Link],
    now: DateTime<Utc>,
    jobs: &mut Vec<Job>,
) -> Result<(), OpError> {
    for link in links {
        if jobs.len() >= BATCH || waiting(link, now) {
            continue;
        }
        drop_or_delete(conn, account, link, jobs)?;
    }
    Ok(())
}

fn drop_or_delete(
    conn: &mut Connection,
    account: &Account,
    link: &Link,
    jobs: &mut Vec<Job>,
) -> Result<(), OpError> {
    match &link.google_event_id {
        Some(google_event_id) => jobs.push(Job::Delete {
            discord_user_id: account.discord_user_id.clone(),
            event_id: link.event_id,
            google_event_id: google_event_id.clone(),
            refresh_token: account.refresh_token.clone(),
        }),
        None => drop_link(conn, link)?,
    }
    Ok(())
}

fn drop_link(conn: &mut Connection, link: &Link) -> Result<(), OpError> {
    diesel::delete(
        schema::google_events::table
            .filter(schema::google_events::discord_user_id.eq(&link.discord_user_id))
            .filter(schema::google_events::event_id.eq(link.event_id)),
    )
    .execute(conn)?;
    Ok(())
}

fn clear_forced(
    conn: &mut Connection,
    discord_user_id: &str,
    event_id: i64,
) -> Result<(), OpError> {
    diesel::update(
        schema::google_events::table
            .filter(schema::google_events::discord_user_id.eq(discord_user_id))
            .filter(schema::google_events::event_id.eq(event_id)),
    )
    .set(schema::google_events::forced.eq(false))
    .execute(conn)?;
    Ok(())
}

pub fn record(conn: &mut Connection, outcome: Outcome) -> Result<(), OpError> {
    match outcome {
        Outcome::Saved {
            discord_user_id,
            event_id,
            google_event_id,
            snapshot,
        } => {
            diesel::insert_into(schema::google_events::table)
                .values((
                    schema::google_events::discord_user_id.eq(&discord_user_id),
                    schema::google_events::event_id.eq(event_id),
                    schema::google_events::google_event_id.eq(&google_event_id),
                    schema::google_events::content_hash.eq(&snapshot),
                    schema::google_events::last_error.eq(""),
                    schema::google_events::retry_at.eq(Option::<String>::None),
                ))
                .on_conflict((
                    schema::google_events::discord_user_id,
                    schema::google_events::event_id,
                ))
                .do_update()
                .set((
                    schema::google_events::google_event_id.eq(&google_event_id),
                    schema::google_events::content_hash.eq(&snapshot),
                    schema::google_events::last_error.eq(""),
                    schema::google_events::retry_at.eq(Option::<String>::None),
                    schema::google_events::forced.eq(false),
                ))
                .execute(conn)?;
        }
        Outcome::Removed {
            discord_user_id,
            event_id,
        } => {
            diesel::delete(
                schema::google_events::table
                    .filter(schema::google_events::discord_user_id.eq(&discord_user_id))
                    .filter(schema::google_events::event_id.eq(event_id)),
            )
            .execute(conn)?;
        }
        Outcome::Failed {
            discord_user_id,
            event_id,
            message,
            auth_dead,
        } => {
            if auth_dead {
                diesel::update(
                    schema::google_accounts::table
                        .filter(schema::google_accounts::discord_user_id.eq(&discord_user_id)),
                )
                .set(schema::google_accounts::last_error.eq(&message))
                .execute(conn)?;
            }
            let retry = clock::stamp(Utc::now() + Duration::seconds(60));
            diesel::insert_into(schema::google_events::table)
                .values((
                    schema::google_events::discord_user_id.eq(&discord_user_id),
                    schema::google_events::event_id.eq(event_id),
                    schema::google_events::content_hash.eq(""),
                    schema::google_events::last_error.eq(&message),
                    schema::google_events::retry_at.eq(&retry),
                ))
                .on_conflict((
                    schema::google_events::discord_user_id,
                    schema::google_events::event_id,
                ))
                .do_update()
                .set((
                    schema::google_events::last_error.eq(&message),
                    schema::google_events::retry_at.eq(&retry),
                ))
                .execute(conn)?;
        }
    }
    Ok(())
}

pub async fn account_email(http: &reqwest::Client, access_token: &str) -> String {
    let Ok(response) = http
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .timeout(StdDuration::from_secs(10))
        .bearer_auth(access_token)
        .send()
        .await
    else {
        return String::new();
    };
    let Ok(body) = response.text().await else {
        return String::new();
    };
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("email")
                .and_then(|email| email.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub async fn exchange_tokens(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<(String, String), String> {
    let (_status, body) = post_form(
        http,
        "https://oauth2.googleapis.com/token",
        &[
            ("code", code),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ],
    )
    .await?;
    let grant = parse_grant(&body).map_err(|err| match err {
        GrantError::Dead(message) | GrantError::Retry(message) => message,
    })?;
    let refresh = grant
        .refresh_token
        .ok_or_else(|| "Google didn't send a calendar credential. Connect again.".to_string())?;
    Ok((refresh, grant.access_token))
}

pub async fn push(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    job: Job,
) -> Outcome {
    let (discord_user_id, event_id, refresh_token) = match &job {
        Job::Upsert {
            discord_user_id,
            event_id,
            refresh_token,
            ..
        }
        | Job::Delete {
            discord_user_id,
            event_id,
            refresh_token,
            ..
        } => (discord_user_id.clone(), *event_id, refresh_token.clone()),
    };
    let access = match access_token(http, client_id, client_secret, &refresh_token).await {
        Ok(token) => token,
        Err(GrantError::Dead(message)) => {
            return Outcome::Failed {
                discord_user_id,
                event_id,
                message,
                auth_dead: true,
            };
        }
        Err(GrantError::Retry(message)) => {
            return Outcome::Failed {
                discord_user_id,
                event_id,
                message,
                auth_dead: false,
            };
        }
    };
    match job {
        Job::Delete {
            google_event_id, ..
        } => match delete_event(http, &access, &google_event_id).await {
            Ok(()) => Outcome::Removed {
                discord_user_id,
                event_id,
            },
            Err((message, auth_dead)) => Outcome::Failed {
                discord_user_id,
                event_id,
                message,
                auth_dead,
            },
        },
        Job::Upsert {
            google_event_id,
            marian_id,
            snapshot,
            body,
            ..
        } => {
            let existing = match google_event_id {
                Some(id) => Some(id),
                None => match find_event(http, &access, &marian_id).await {
                    Ok(id) => id,
                    Err((message, auth_dead)) => {
                        return Outcome::Failed {
                            discord_user_id,
                            event_id,
                            message,
                            auth_dead,
                        };
                    }
                },
            };
            match write_event(http, &access, existing.as_deref(), &body).await {
                Ok(google_event_id) => Outcome::Saved {
                    discord_user_id,
                    event_id,
                    google_event_id,
                    snapshot,
                },
                Err((message, auth_dead)) => Outcome::Failed {
                    discord_user_id,
                    event_id,
                    message,
                    auth_dead,
                },
            }
        }
    }
}

pub async fn forget_remote(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    access: RemoteAccess,
) -> bool {
    let token = match access_token(http, client_id, client_secret, &access.refresh_token).await {
        Ok(token) => token,
        Err(GrantError::Dead(_)) => {
            let _ = revoke(http, &access.refresh_token).await;
            return true;
        }
        Err(GrantError::Retry(_)) => return false,
    };
    let mut clean = true;
    for id in &access.google_event_ids {
        if delete_event(http, &token, id).await.is_err() {
            clean = false;
        }
    }
    let _ = revoke(http, &access.refresh_token).await;
    clean
}

fn account(conn: &mut Connection, discord_user_id: &str) -> Result<Option<Account>, OpError> {
    Ok(schema::google_accounts::table
        .filter(schema::google_accounts::discord_user_id.eq(discord_user_id))
        .select(Account::as_select())
        .first(conn)
        .optional()?)
}

fn links_for(conn: &mut Connection, discord_user_id: &str) -> Result<Vec<Link>, OpError> {
    Ok(schema::google_events::table
        .filter(schema::google_events::discord_user_id.eq(discord_user_id))
        .select(Link::as_select())
        .load(conn)?)
}

fn waiting(link: &Link, now: DateTime<Utc>) -> bool {
    link.retry_at
        .as_deref()
        .and_then(|stamp| clock::parse_stamp(stamp).ok())
        .is_some_and(|at| at > now)
}

struct Payload {
    snapshot: String,
    body: serde_json::Value,
}

fn payload(conn: &mut Connection, event: &Event, public_base: &str) -> Result<Payload, OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let end = start + Duration::minutes(event.duration_minutes);
    let channel = store::channel(conn, event.channel_id)?
        .map(|channel| format!("#{}", channel.name))
        .unwrap_or_default();
    let page = web_page(conn, event, public_base);
    let description = [event.description.trim(), channel.as_str(), page.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let description: String = description.chars().take(8000).collect();
    let start_at = google_time(start);
    let end_at = google_time(end);
    let snapshot = format!(
        "{}\n{}\n{start_at}\n{end_at}\n{description}\n{channel}",
        event.public_id, event.title
    );
    let body = json!({
        "summary": event.title,
        "description": description,
        "location": channel,
        "start": {"dateTime": start_at, "timeZone": "UTC"},
        "end": {"dateTime": end_at, "timeZone": "UTC"},
        "extendedProperties": {"private": {"marian": event.public_id}},
    });
    Ok(Payload { snapshot, body })
}

fn web_page(conn: &mut Connection, event: &Event, public_base: &str) -> String {
    let base = public_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    let Ok(server) = store::discord_server_id(conn, event.guild_id) else {
        return String::new();
    };
    format!("{base}/servers/{server}/events/{}", event.public_id)
}

fn google_time(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn parse_grant(body: &str) -> Result<Grant, GrantError> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| GrantError::Retry("Google sent an unreadable token response.".into()))?;
    if let Some(token) = parsed
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
    {
        let refresh = parsed
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        return Ok(Grant {
            access_token: token.to_string(),
            refresh_token: refresh,
        });
    }
    let code = parsed
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if code == "invalid_grant" {
        Err(GrantError::Dead(
            "Google Calendar needs a new connection.".into(),
        ))
    } else {
        Err(GrantError::Retry(
            "Google Calendar didn't grant access.".into(),
        ))
    }
}

fn parse_event_id(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("id")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn parse_search_id(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("items")?
        .as_array()?
        .iter()
        .find_map(|item| {
            item.get("id")
                .and_then(|id| id.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
}

async fn access_token(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String, GrantError> {
    let (_status, body) = post_form(
        http,
        "https://oauth2.googleapis.com/token",
        &[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )
    .await
    .map_err(GrantError::Retry)?;
    parse_grant(&body).map(|grant| grant.access_token)
}

async fn find_event(
    http: &reqwest::Client,
    access: &str,
    marian_id: &str,
) -> Result<Option<String>, (String, bool)> {
    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/primary/events?privateExtendedProperty={}&maxResults=1&showDeleted=false",
        auth::encode(&format!("marian={marian_id}"))
    );
    let (status, body) = send(http.get(&url).bearer_auth(access))
        .await
        .map_err(|message| (message, false))?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    Ok(parse_search_id(&body))
}

async fn write_event(
    http: &reqwest::Client,
    access: &str,
    google_event_id: Option<&str>,
    body: &serde_json::Value,
) -> Result<String, (String, bool)> {
    if let Some(id) = google_event_id {
        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/primary/events/{}",
            auth::encode(id)
        );
        let (status, text) = send(http.patch(&url).bearer_auth(access).json(body))
            .await
            .map_err(|message| (message, false))?;
        if status.is_success() {
            return parse_event_id(&text).ok_or_else(|| {
                (
                    "Google Calendar returned an event without an id.".into(),
                    false,
                )
            });
        }
        if status != reqwest::StatusCode::NOT_FOUND {
            return Err(api_error(status, &text));
        }
    }
    let (status, text) = send(
        http.post("https://www.googleapis.com/calendar/v3/calendars/primary/events")
            .bearer_auth(access)
            .json(body),
    )
    .await
    .map_err(|message| (message, false))?;
    if !status.is_success() {
        return Err(api_error(status, &text));
    }
    parse_event_id(&text).ok_or_else(|| {
        (
            "Google Calendar returned an event without an id.".into(),
            false,
        )
    })
}

async fn delete_event(
    http: &reqwest::Client,
    access: &str,
    google_event_id: &str,
) -> Result<(), (String, bool)> {
    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/primary/events/{}",
        auth::encode(google_event_id)
    );
    let (status, body) = send(http.delete(&url).bearer_auth(access))
        .await
        .map_err(|message| (message, false))?;
    if status.is_success()
        || status == reqwest::StatusCode::NOT_FOUND
        || status == reqwest::StatusCode::GONE
    {
        return Ok(());
    }
    Err(api_error(status, &body))
}

async fn revoke(http: &reqwest::Client, refresh_token: &str) -> Result<(), String> {
    post_form(
        http,
        "https://oauth2.googleapis.com/revoke",
        &[("token", refresh_token)],
    )
    .await
    .map(|_| ())
}

async fn post_form(
    http: &reqwest::Client,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, String), String> {
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}={}", auth::encode(key), auth::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    send(
        http.post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body),
    )
    .await
}

async fn send(builder: reqwest::RequestBuilder) -> Result<(reqwest::StatusCode, String), String> {
    let response = builder
        .timeout(StdDuration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Google Calendar didn't respond.".to_string())?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| "Google Calendar didn't respond.".to_string())?;
    Ok((status, body))
}

fn api_error(status: reqwest::StatusCode, body: &str) -> (String, bool) {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let code = parsed
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let message = if !code.is_empty() {
        code.to_string()
    } else {
        parsed
            .pointer("/error/message")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    };
    let auth_dead = code == "invalid_grant"
        || parsed
            .pointer("/error/status")
            .and_then(|value| value.as_str())
            == Some("UNAUTHENTICATED");
    let message = if message.is_empty() {
        format!("Google Calendar returned {}.", status.as_u16())
    } else {
        message.chars().take(180).collect()
    };
    (message, auth_dead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;

    #[test]
    fn authorize_url_asks_for_calendar_events() {
        let url = authorize_url(
            "client",
            "https://marian.example/auth/google/callback",
            "abcdef0123456789",
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=client"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("calendar.events"));
        assert!(url.contains("userinfo.email"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fmarian.example%2Fauth%2Fgoogle%2Fcallback")
        );
        assert_eq!(
            redirect_uri("https://marian.example/", None, None).as_deref(),
            Some("https://marian.example/auth/google/callback")
        );
    }

    #[test]
    fn a_sign_in_link_belongs_to_one_discord_account() {
        let mut conn = store::open_memory().unwrap();
        let state = begin_link(&mut conn, "42").unwrap();
        let url = authorize_url(
            "1234567890-abcdefghijklmnopqrstuvwxyz.apps.googleusercontent.com",
            "https://marian.example/auth/google/callback",
            &state,
        );
        assert!(url.len() <= 512, "{}", url.len());
        assert!(url.contains(&state));
        assert_eq!(
            claim_oauth(&mut conn, &state).unwrap().as_deref(),
            Some("42")
        );
        finish_oauth(&mut conn, &state).unwrap();
        assert!(claim_oauth(&mut conn, &state).unwrap().is_none());
    }

    #[test]
    fn a_grant_keeps_the_refresh_token_and_hides_it_from_errors() {
        let grant = match parse_grant(
            r#"{"access_token":"ya29.access","refresh_token":"1//refresh","expires_in":3600}"#,
        ) {
            Ok(grant) => grant,
            Err(_) => panic!("expected a token grant"),
        };
        assert_eq!(grant.access_token, "ya29.access");
        assert_eq!(grant.refresh_token.as_deref(), Some("1//refresh"));
        let err = match parse_grant(r#"{"error":"invalid_grant"}"#) {
            Err(err) => err,
            Ok(_) => panic!("expected a dead grant"),
        };
        let GrantError::Dead(message) = err else {
            panic!("expected a dead grant");
        };
        assert!(!message.contains("refresh"));
        assert_eq!(
            parse_event_id(r#"{"id":"evt123","summary":"Raid"}"#).as_deref(),
            Some("evt123")
        );
        assert_eq!(
            parse_search_id(r#"{"items":[{"id":"evt123"}]}"#).as_deref(),
            Some("evt123")
        );
    }

    #[test]
    fn a_connected_calendar_tracks_created_and_accepted_events() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99');
            INSERT INTO channels (id, guild_id, name) VALUES (1, 1, 'social');
            INSERT INTO users (id, guild_id, name, handle, color, discord_id) VALUES
              (1, 1, 'Nell', 'nell', '#112233', 'u1'),
              (2, 1, 'June', 'june', '#112233', 'u2');
            INSERT INTO events (public_id, guild_id, channel_id, title, description, starts_at, duration_minutes, status, created_by, created_at, updated_at) VALUES
              ('01900000-0000-7000-8000-000000000001', 1, 1, 'Nell hosts', 'Bring dice.', '2026-10-14T18:00:00Z', 90, 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000002', 1, 1, 'June hosts accepted', '', '2026-10-15T18:00:00Z', 60, 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'),
              ('01900000-0000-7000-8000-000000000003', 1, 1, 'June hosts declined', '', '2026-10-16T18:00:00Z', 60, 'posted', 2, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z');
            INSERT INTO signup_options (id, event_id, name, emoji, reminders_enabled, position) VALUES
              (1, 2, 'Accepted', '✅', 1, 0),
              (2, 3, 'Declined', '❌', 0, 0);
            INSERT INTO signups (event_id, option_id, user_id, state, created_at) VALUES
              (2, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (3, 2, 1, 'confirmed', '2026-10-13T00:00:00Z');
            INSERT INTO google_accounts (discord_user_id, refresh_token, email, connected_at)
              VALUES ('u1', 'refresh-token', 'nell@example.com', '2026-10-15T00:00:00Z');
            ",
        )
        .unwrap();
        let jobs = plan(&mut conn, "https://marian.example").unwrap();
        assert_eq!(jobs.len(), 2);
        let titles = jobs
            .iter()
            .map(|job| match job {
                Job::Upsert {
                    body,
                    refresh_token,
                    ..
                } => {
                    assert_eq!(refresh_token, "refresh-token");
                    assert!(!body.to_string().contains("refresh-token"));
                    body.get("summary")
                        .and_then(|value| value.as_str())
                        .unwrap()
                        .to_string()
                }
                Job::Delete { .. } => panic!("a new calendar creates events"),
            })
            .collect::<Vec<_>>();
        assert!(titles.contains(&"Nell hosts".to_string()), "{titles:?}");
        assert!(
            titles.contains(&"June hosts accepted".to_string()),
            "{titles:?}"
        );
        assert!(!titles.iter().any(|title| title.contains("declined")));
        let nell = jobs.iter().find(|job| matches!(job, Job::Upsert { body, .. } if body.get("summary").and_then(|v| v.as_str()) == Some("Nell hosts"))).unwrap();
        let Job::Upsert {
            event_id,
            snapshot,
            body,
            ..
        } = nell
        else {
            panic!("expected an upsert");
        };
        assert!(body
            .get("description")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Bring dice."));
        assert!(body.get("description").unwrap().as_str().unwrap().contains(
            "https://marian.example/servers/99/events/01900000-0000-7000-8000-000000000001"
        ));
        record(
            &mut conn,
            Outcome::Saved {
                discord_user_id: "u1".into(),
                event_id: *event_id,
                google_event_id: "g-nell".into(),
                snapshot: snapshot.clone(),
            },
        )
        .unwrap();
        let other = jobs
            .iter()
            .find(|job| matches!(job, Job::Upsert { event_id: id, .. } if id != event_id))
            .unwrap();
        let Job::Upsert {
            event_id: other_id,
            snapshot: other_snapshot,
            ..
        } = other
        else {
            panic!("expected an upsert");
        };
        record(
            &mut conn,
            Outcome::Saved {
                discord_user_id: "u1".into(),
                event_id: *other_id,
                google_event_id: "g-june".into(),
                snapshot: other_snapshot.clone(),
            },
        )
        .unwrap();
        assert!(plan(&mut conn, "https://marian.example")
            .unwrap()
            .is_empty());
        assert!(on_calendar(&mut conn, "u1", *event_id).unwrap());

        diesel::update(schema::events::table.filter(schema::events::id.eq(*event_id)))
            .set(schema::events::title.eq("Nell hosts later"))
            .execute(&mut conn)
            .unwrap();
        let jobs = plan(&mut conn, "https://marian.example").unwrap();
        assert_eq!(jobs.len(), 1);
        match &jobs[0] {
            Job::Upsert {
                google_event_id,
                body,
                ..
            } => {
                assert_eq!(google_event_id.as_deref(), Some("g-nell"));
                assert_eq!(
                    body.get("summary").and_then(|value| value.as_str()),
                    Some("Nell hosts later")
                );
            }
            Job::Delete { .. } => panic!("a title change updates the event"),
        }

        record(
            &mut conn,
            Outcome::Failed {
                discord_user_id: "u1".into(),
                event_id: *event_id,
                message: "Google Calendar didn't respond.".into(),
                auth_dead: false,
            },
        )
        .unwrap();
        assert!(plan(&mut conn, "https://marian.example")
            .unwrap()
            .is_empty());

        diesel::update(schema::events::table.filter(schema::events::status.eq("posted")))
            .set(schema::events::status.eq("draft"))
            .execute(&mut conn)
            .unwrap();
        diesel::update(
            schema::google_events::table.filter(schema::google_events::event_id.eq(*event_id)),
        )
        .set(schema::google_events::retry_at.eq(Option::<String>::None))
        .execute(&mut conn)
        .unwrap();
        let jobs = plan(&mut conn, "https://marian.example").unwrap();
        assert!(jobs.iter().any(
            |job| matches!(job, Job::Delete { google_event_id, .. } if google_event_id == "g-nell")
        ));
        assert!(jobs.iter().any(
            |job| matches!(job, Job::Delete { google_event_id, .. } if google_event_id == "g-june")
        ));

        record(
            &mut conn,
            Outcome::Failed {
                discord_user_id: "u1".into(),
                event_id: *event_id,
                message: "Google Calendar needs a new connection.".into(),
                auth_dead: true,
            },
        )
        .unwrap();
        assert!(plan(&mut conn, "https://marian.example")
            .unwrap()
            .is_empty());
        let status = status(&mut conn, "u1").unwrap().unwrap();
        assert!(status.reconnect);
        assert!(!status.connected);
    }
}
