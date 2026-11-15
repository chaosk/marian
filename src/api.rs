//! What the web pages and the REST API read and change, as plain resources.
//!
//! Each function works for one signed-in person bound to one server.

use std::collections::HashMap;

use axum::http::{HeaderMap, header};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use marian_types as dto;

use crate::clock;
use crate::error::OpError;
use crate::ics;
use crate::ids;
use crate::jobs::DiscordJob;
use crate::logic::{self, EventInput, OptionInput, ReminderInput, RepeatSpec};
use crate::store::{self, Connection};

pub use crate::observe::PageTrace;
use crate::values::{Cadence, Digest, EventRoleKind, EventStatus, SignupState};

/// What server-rendered pages need from the running service.
#[derive(Clone)]
pub struct Web {
    pub db: store::Db,
    pub invite_url: String,
    pub oauth_ready: bool,
}

/// A signed-in person, bound to the server they're looking at.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub token: String,
    pub discord_user_id: String,
    pub user_id: i64,
    pub display_name: String,
    pub guild_id: i64,
    pub membership_id: i64,
    pub allowed: Vec<i64>,
    pub notice: Option<String>,
}

/// A change's reply, and the Discord work it leaves behind.
#[derive(Debug, Default)]
pub struct Outcome {
    pub done: dto::Done,
    /// An event whose Discord post needs refreshing.
    pub sync: Option<i64>,
    pub jobs: Vec<DiscordJob>,
}

impl Outcome {
    fn ok() -> Self {
        Self::default()
    }

    fn at(mut self, location: String) -> Self {
        self.done.location = Some(location);
        self
    }

    fn synced(mut self, event_id: i64) -> Self {
        self.sync = Some(event_id);
        self
    }
}

pub async fn viewer(conn: &mut Connection, headers: &HeaderMap) -> Option<Viewer> {
    let token = cookie(headers, "marian_session")?;
    let row = store::web_session(conn, &token).await.ok()??;
    let fresh = Utc::now() - row.created_at < Duration::days(store::SESSION_DAYS);
    if !fresh {
        let _ = store::delete_web_session(conn, &token).await;
        return None;
    }
    let user = store::membership(conn, row.membership_id).await.ok()??;
    let discord_id = store::discord_id_for_membership(conn, user.id).await.ok()?;
    if user.guild_id != row.guild_id || discord_id != row.discord_user_id {
        let _ = store::delete_web_session(conn, &token).await;
        return None;
    }
    let notice = cookie(headers, "marian_notice").map(|value| percent_decode(&value));
    Some(Viewer {
        token,
        discord_user_id: row.discord_user_id,
        user_id: user.user_id,
        display_name: row.display_name,
        guild_id: row.guild_id,
        membership_id: row.membership_id,
        allowed: row.guilds,
        notice,
    })
}

/// Points the viewer at the server with this Discord id, for this request.
pub async fn bind_server(
    conn: &mut Connection,
    viewer: &mut Viewer,
    server: &str,
) -> Result<(), OpError> {
    let guild = store::guild_by_discord(conn, server)
        .await?
        .ok_or(OpError::NotFound)?;
    bind_guild(conn, viewer, guild.id).await
}

/// A server the viewer didn't sign into reads as one that isn't there, so
/// nobody can learn which servers Marian is in by asking for them.
pub async fn bind_guild(
    conn: &mut Connection,
    viewer: &mut Viewer,
    guild_id: i64,
) -> Result<(), OpError> {
    if guild_id <= 0
        || !viewer.allowed.contains(&guild_id)
        || store::guild(conn, guild_id).await?.is_none()
    {
        return Err(OpError::NotFound);
    }
    let user = logic::admit_discord_user(
        conn,
        guild_id,
        &viewer.discord_user_id,
        &viewer.display_name,
    )
    .await?;
    viewer.guild_id = guild_id;
    viewer.membership_id = user.id;
    Ok(())
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let mut bits = part.trim().splitn(2, '=');
        if bits.next() == Some(name) {
            return bits.next().map(|value| value.to_string());
        }
    }
    None
}

pub fn percent_decode(text: &str) -> String {
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

async fn server_id(conn: &mut Connection, viewer: &Viewer) -> Result<String, OpError> {
    store::discord_server_id(conn, viewer.guild_id).await
}

async fn path(conn: &mut Connection, viewer: &Viewer, rest: &str) -> Result<String, OpError> {
    Ok(format!("/servers/{}{rest}", server_id(conn, viewer).await?))
}

async fn zone(conn: &mut Connection, guild_id: i64) -> Result<Tz, OpError> {
    let guild = store::guild(conn, guild_id)
        .await?
        .ok_or(OpError::NotFound)?;
    clock::zone(&guild.timezone)
}

async fn must_member(conn: &mut Connection, viewer: &Viewer) -> Result<store::Membership, OpError> {
    store::membership(conn, viewer.membership_id)
        .await?
        .ok_or(OpError::NotFound)
}

/// The viewer, if they can manage the server. Everyone else sees only the
/// events they have access to, not the server's routines, series, activity,
/// or commands.
async fn must_manager(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<store::Membership, OpError> {
    let user = must_member(conn, viewer).await?;
    if !user.server_manager {
        return Err(OpError::message(
            "Only someone who runs the server can see this.",
        ));
    }
    Ok(user)
}

/// Which of the server's channels the viewer can see in Discord.
async fn sight(conn: &mut Connection, viewer: &Viewer) -> Result<logic::Sight, OpError> {
    let user = must_member(conn, viewer).await?;
    logic::sight(conn, &user).await
}

/// An event in the viewer's server, in a channel they can see.
async fn event_here(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<store::Event, OpError> {
    let event = store::event_by_public(conn, public_id)
        .await?
        .ok_or(OpError::NotFound)?;
    if event.guild_id != viewer.guild_id || !sight(conn, viewer).await?.sees_event(&event) {
        return Err(OpError::NotFound);
    }
    Ok(event)
}

/// The channel the events made from a template go to.
async fn template_channel(conn: &mut Connection, template_id: i64) -> Result<i64, OpError> {
    use diesel::prelude::*;
    use diesel_async::RunQueryDsl;
    Ok(crate::schema::event_templates::table
        .find(template_id)
        .select(crate::schema::event_templates::channel_id)
        .first(conn)
        .await?)
}

async fn series_here(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<store::Series, OpError> {
    let series = store::series_by_public(conn, public_id)
        .await?
        .ok_or(OpError::NotFound)?;
    if series.guild_id != viewer.guild_id
        || !sight(conn, viewer)
            .await?
            .sees_channel(template_channel(conn, series.template_id).await?)
    {
        return Err(OpError::NotFound);
    }
    Ok(series)
}

/// A routine in the viewer's server, posting in a channel they can see.
async fn routine_here(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<i64, OpError> {
    use diesel::prelude::*;
    use diesel_async::RunQueryDsl;
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id).await?;
    let template_id: i64 = crate::schema::routines::table
        .find(id)
        .select(crate::schema::routines::template_id)
        .first(conn)
        .await?;
    if !sight(conn, viewer)
        .await?
        .sees_channel(template_channel(conn, template_id).await?)
    {
        return Err(OpError::NotFound);
    }
    Ok(id)
}

/// A channel the viewer picked, if it's in their server and they can see it.
async fn channel_seen(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<i64, OpError> {
    let id = ids::public::channel_id(conn, viewer.guild_id, public_id).await?;
    if !sight(conn, viewer).await?.sees_channel(id) {
        return Err(OpError::message("Pick a channel."));
    }
    Ok(id)
}

pub async fn me(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Me, OpError> {
    let servers = store::list_guilds(conn)
        .await?
        .into_iter()
        .filter(|guild| viewer.allowed.contains(&guild.id))
        .filter_map(|guild| {
            let id = guild.discord_id.filter(|id| !id.is_empty())?;
            Some(dto::ServerRef {
                id,
                name: guild.name,
            })
        })
        .collect();
    Ok(dto::Me {
        name: viewer.display_name.clone(),
        servers,
    })
}

pub async fn server(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Server, OpError> {
    let guild = store::guild(conn, viewer.guild_id)
        .await?
        .ok_or(OpError::NotFound)?;
    let settings = store::settings(conn, viewer.guild_id).await?;
    let counts = dto::Counts {
        drafts: store::count_status(conn, viewer.guild_id, EventStatus::Draft).await?,
        upcoming: store::count_upcoming(conn, viewer.guild_id).await?,
        scheduled: store::count_status(conn, viewer.guild_id, EventStatus::Scheduled).await?
            + store::count_status(conn, viewer.guild_id, EventStatus::Failed).await?,
        series: store::count_active_series(conn, viewer.guild_id).await?,
        routines: logic::count_active(conn, viewer.guild_id).await?,
    };
    Ok(dto::Server {
        id: guild.discord_id.unwrap_or_default(),
        name: guild.name,
        timezone: guild.timezone,
        hour_24: settings.hour_24,
        localize_time: settings.localize_time,
        counts,
    })
}

pub async fn membership(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<dto::Membership, OpError> {
    let user = must_member(conn, viewer).await?;
    let token = store::ensure_calendar_token(conn, user.user_id).await?;
    let settings = store::settings(conn, user.guild_id).await?;
    let roles = store::membership_role_ids(conn, &user).await?;
    let can_create_event = crate::logic::can_create(&user, &settings, &roles);
    Ok(dto::Membership {
        name: user.name,
        avatar: user.avatar,
        server_manager: user.server_manager,
        can_create_event,
        reminders_enabled: user.reminders_enabled,
        calendar_url: calendar_feed_url(
            &std::env::var("MARIAN_PUBLIC_URL").unwrap_or_default(),
            &token,
        ),
        google: google(conn, user.user_id).await?,
    })
}

/// A person's calendar feed: its full address when the server knows its own,
/// else a path the browser completes with the address it's on.
fn calendar_feed_url(public_url: &str, token: &str) -> String {
    format!(
        "{}/calendar/{token}.ics",
        public_url.trim().trim_end_matches('/')
    )
}

async fn google(conn: &mut Connection, user_id: i64) -> Result<dto::Google, OpError> {
    let status = crate::google::status(conn, user_id).await?;
    Ok(dto::Google {
        ready: crate::google::configured_from_env(),
        connected: status.as_ref().is_some_and(|s| s.connected),
        reconnect: status.as_ref().is_some_and(|s| s.reconnect),
        auto_sync: status.as_ref().is_some_and(|s| s.auto_sync),
        email: status.map(|s| s.email).unwrap_or_default(),
    })
}

pub async fn patch_membership(
    conn: &mut Connection,
    viewer: &Viewer,
    patch: &dto::MembershipPatch,
) -> Result<Outcome, OpError> {
    if let Some(enabled) = patch.reminders_enabled {
        logic::set_user_reminders(conn, viewer.membership_id, enabled).await?;
    }
    if let Some(enabled) = patch.google_auto_sync {
        crate::commands::google_auto_sync(
            conn,
            viewer.guild_id,
            viewer.membership_id,
            enabled,
            crate::i18n::Lang::En,
        )
        .await?;
    }
    Ok(Outcome::ok())
}

pub async fn settings(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Settings, OpError> {
    let guild = store::guild(conn, viewer.guild_id)
        .await?
        .ok_or(OpError::NotFound)?;
    let s = store::settings(conn, viewer.guild_id).await?;
    let public = ids::public::roles;
    Ok(dto::Settings {
        role_event: public(conn, &s.role_event).await?,
        role_edit: public(conn, &s.role_edit).await?,
        role_delete: public(conn, &s.role_delete).await?,
        role_channel: public(conn, &s.role_channel).await?,
        role_signup: public(conn, &s.role_signup).await?,
        timezone: guild.timezone,
        default_reminder_minutes: s.default_reminder_minutes,
        auto_archive_hours: s.auto_archive_hours,
        post_lead_hours: s.post_lead_hours,
        digest: s.digest.to_string(),
        thread_reminders: s.thread_reminders,
        auto_archive: s.auto_archive,
        event_threads: s.event_threads,
        auto_join_threads: s.auto_join_threads,
        show_attendees: s.show_attendees,
        hour_24: s.hour_24,
        localize_time: s.localize_time,
        purge_channels: s.purge_channels,
        temp_roles: s.temp_roles,
        waitlist: s.waitlist,
        calendar_auto_sync: s.calendar_auto_sync,
        language: s.language.code().to_string(),
    })
}

pub async fn put_settings(
    conn: &mut Connection,
    viewer: &Viewer,
    next: &dto::Settings,
) -> Result<Outcome, OpError> {
    let user = must_member(conn, viewer).await?;
    if !user.server_manager {
        return Err(OpError::message(
            "Only someone who runs the server can change settings.",
        ));
    }
    clock::zone(&next.timezone)
        .map_err(|_| OpError::message("That timezone isn't one Marian knows."))?;
    let mut s = store::settings(conn, viewer.guild_id).await?;
    // Zero is "no default reminder".
    s.default_reminder_minutes = bounded(
        "The default reminder",
        next.default_reminder_minutes,
        0,
        10080,
    )?;
    s.auto_archive_hours = bounded("Archive after", next.auto_archive_hours, 1, 168)?;
    s.post_lead_hours = bounded("Post lead", next.post_lead_hours, 0, 336)?;
    s.digest = match next.digest.as_str() {
        "daily" => Digest::Daily,
        "weekly" => Digest::Weekly,
        _ => Digest::Off,
    };
    // Roles by public id. One the server no longer has is dropped.
    let roles: std::collections::HashMap<String, i64> = store::list_roles(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|role| (role.public_id.to_string(), role.id))
        .collect();
    let known = |ids: &[String]| -> Vec<i64> {
        ids.iter().filter_map(|id| roles.get(id).copied()).collect()
    };
    s.role_event = known(&next.role_event);
    s.role_edit = known(&next.role_edit);
    s.role_delete = known(&next.role_delete);
    s.role_channel = known(&next.role_channel);
    s.role_signup = known(&next.role_signup);
    s.thread_reminders = next.thread_reminders;
    s.auto_archive = next.auto_archive;
    s.event_threads = next.event_threads;
    s.auto_join_threads = next.auto_join_threads;
    s.show_attendees = next.show_attendees;
    s.hour_24 = next.hour_24;
    s.localize_time = next.localize_time;
    s.purge_channels = next.purge_channels;
    s.temp_roles = next.temp_roles;
    s.waitlist = next.waitlist;
    s.calendar_auto_sync = next.calendar_auto_sync;
    let outcome = Outcome::ok();
    if !next.language.is_empty() {
        let picked = crate::i18n::Lang::from_code(&next.language)
            .ok_or_else(|| OpError::message("That language isn't one Marian writes in."))?;
        // The slash commands list every language, so nothing to register.
        s.language = picked;
    }
    store::with_tx(conn, async |tx| {
        store::set_guild_timezone(tx, viewer.guild_id, &next.timezone).await?;
        store::update_settings(tx, &s).await
    })
    .await?;
    Ok(outcome)
}

fn bounded(label: &str, value: i64, min: i64, max: i64) -> Result<i64, OpError> {
    if !(min..=max).contains(&value) {
        return Err(OpError::message(format!(
            "{label} needs to be between {min} and {max}."
        )));
    }
    Ok(value)
}

pub fn timezones(selected: &str) -> Vec<dto::ZoneGroup> {
    clock::zone_groups(Utc::now(), selected)
}

pub async fn roles(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Role>, OpError> {
    let member = store::membership(conn, viewer.membership_id)
        .await?
        .ok_or(OpError::NotFound)?;
    Ok(store::list_roles(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|role| dto::Role {
            attendee: logic::may_pick_attendee_role(&member, &role),
            id: role.public_id.to_string(),
            name: role.name,
        })
        .collect())
}

pub async fn members(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Member>, OpError> {
    Ok(store::list_memberships(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|person| dto::Member {
            id: person.public_id.to_string(),
            name: person.name,
            color: person.color,
            avatar: person.avatar,
        })
        .collect())
}

/// Someone a search of the Discord server found: their Discord id, the name
/// and the picture they show there.
pub struct FoundMember {
    pub discord_id: String,
    pub name: String,
    pub avatar: Option<String>,
}

/// People found in the Discord server, as members here. Anyone new gets the
/// membership they would get the first time they pressed a button. With them
/// come the events whose posts still show someone's old name.
pub async fn remember_members(
    conn: &mut Connection,
    viewer: &Viewer,
    found: &[FoundMember],
) -> Result<(Vec<dto::Member>, Vec<i64>), OpError> {
    let mut members = Vec::new();
    let mut redraw = Vec::new();
    for found in found {
        let (person, renamed) =
            logic::ensure_discord_user(conn, viewer.guild_id, &found.discord_id, &found.name)
                .await?;
        redraw.extend(renamed);
        logic::follow_discord_avatar(conn, &person, found.avatar.as_deref()).await?;
        members.push(dto::Member {
            id: person.public_id.to_string(),
            name: person.name,
            color: person.color,
            avatar: found.avatar.clone(),
        });
    }
    Ok((members, redraw))
}

pub async fn channels(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<Vec<dto::Channel>, OpError> {
    let sight = sight(conn, viewer).await?;
    let events: Vec<store::Event> = store::list_events(conn, viewer.guild_id)
        .await?
        .into_iter()
        .filter(|event| sight.sees_event(event))
        .collect();
    Ok(store::list_channels(conn, viewer.guild_id)
        .await?
        .into_iter()
        .filter(|channel| sight.sees_channel(channel.id))
        .map(|channel| dto::Channel {
            events: events.iter().filter(|e| e.channel_id == channel.id).count() as i64,
            id: channel.public_id.to_string(),
            name: channel.name,
            kind: channel.kind,
            purged_at: channel.purged_at,
        })
        .collect())
}

fn matches_view(view: dto::EventView, event: &store::Event, now: DateTime<Utc>) -> bool {
    let ended = event.starts_at + Duration::minutes(event.duration_minutes) < now;
    match view {
        dto::EventView::Upcoming => match event.status {
            EventStatus::Posted => !ended,
            EventStatus::Scheduled => true,
            _ => false,
        },
        dto::EventView::Wrapped => event.status == EventStatus::Posted && ended,
        dto::EventView::Drafts => event.status == EventStatus::Draft,
        dto::EventView::Queued => {
            matches!(event.status, EventStatus::Scheduled | EventStatus::Failed)
        }
        dto::EventView::Archive => {
            matches!(
                event.status,
                EventStatus::Archived | EventStatus::Cancelled | EventStatus::Skipped
            )
        }
        dto::EventView::All => true,
    }
}

pub const PER_PAGE: u32 = 25;

pub async fn events(
    conn: &mut Connection,
    viewer: &Viewer,
    query: &dto::EventQuery,
) -> Result<dto::Paged<dto::EventSummary>, OpError> {
    let now = Utc::now();
    let q = query.q.trim().to_lowercase();
    let series_id = match query.series.as_deref() {
        Some(id) => Some(series_here(conn, viewer, id).await?.id),
        None => None,
    };
    let statuses: Vec<&str> = query
        .status
        .split(',')
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .collect();
    let tz = zone(conn, viewer.guild_id).await?;
    let midnight = |day: chrono::NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|naive| naive.and_local_timezone(tz).earliest())
            .map(|at| at.with_timezone(&Utc))
    };
    let from_day = query.from_day.and_then(midnight);
    let to_day = query
        .to_day
        .and_then(|day| day.succ_opt())
        .and_then(midnight);
    let sight = sight(conn, viewer).await?;
    let mut events: Vec<store::Event> = match series_id {
        Some(id) => store::events_in_series(conn, id).await?,
        None => store::list_events(conn, viewer.guild_id).await?,
    }
    .into_iter()
    .filter(|event| sight.sees_event(event))
    .filter(|event| {
        series_id.is_some()
            || if statuses.is_empty() {
                matches_view(query.view, event, now)
            } else {
                statuses.contains(&event.status.as_str())
            }
    })
    .filter(|event| {
        let ended = event.starts_at + Duration::minutes(event.duration_minutes) < now;
        match query.when {
            None => true,
            Some(dto::EventWhen::Upcoming) => !ended,
            Some(dto::EventWhen::Past) => ended,
        }
    })
    .filter(|event| query.from.is_none_or(|from| event.starts_at >= from))
    .filter(|event| query.to.is_none_or(|to| event.starts_at < to))
    .filter(|event| from_day.is_none_or(|from| event.starts_at >= from))
    .filter(|event| to_day.is_none_or(|to| event.starts_at < to))
    .filter(|event| q.is_empty() || event.title.to_lowercase().contains(&q))
    .collect();
    if query.when == Some(dto::EventWhen::Past) {
        events.reverse();
    }
    let per_page = query.limit.unwrap_or(PER_PAGE).clamp(1, 100);
    let total = events.len();
    let pages = (total as u32).div_ceil(per_page).max(1);
    let page = query.page.unwrap_or(1).clamp(1, pages);
    let start = ((page - 1) * per_page) as usize;
    let shown: Vec<store::Event> = events
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .collect();
    let options = store::options_by_event(conn, &shown).await?;
    let signups = store::signups_by_event(conn, &shown).await?;
    let channels: HashMap<i64, String> = store::all_channels(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|channel| (channel.id, channel.name))
        .collect();
    let items = shown
        .iter()
        .enumerate()
        .map(|(n, event)| dto::EventSummary {
            id: event.public_id.to_string(),
            title: event.title.clone(),
            description: event.description.chars().take(180).collect(),
            status: event.status,
            starts_at: event.starts_at,
            duration_minutes: event.duration_minutes,
            post_at: event.post_at,
            color: event.color.clone(),
            channel: channels
                .get(&event.channel_id)
                .cloned()
                .unwrap_or_else(|| "deleted".into()),
            spots: spots(&options[n], &signups[n]),
        })
        .collect();
    Ok(dto::Paged {
        items,
        total,
        page,
        pages,
        per_page,
    })
}

fn spots(options: &[store::SignupOption], signups: &[store::Signup]) -> Vec<dto::Spot> {
    options
        .iter()
        .map(|opt| dto::Spot {
            emoji: opt.emoji.clone(),
            name: opt.name.clone(),
            capacity: opt.capacity,
            confirmed: places(signups, opt.id, SignupState::Confirmed),
            waiting: places(signups, opt.id, SignupState::Waitlisted),
        })
        .collect()
}

pub async fn event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<dto::Event, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let settings = store::settings(conn, viewer.guild_id).await?;
    let user = must_member(conn, viewer).await?;
    let roles = store::membership_role_ids(conn, &user).await?;
    let can_edit = logic::can_edit(&user, &event, &settings, &roles);
    let channel = store::channel(conn, event.channel_id).await?;
    let channel_name = channel
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "deleted".into());
    let server = server_id(conn, viewer).await?;
    let discord_url = (|| {
        let channel_id = channel.as_ref()?.discord_id.as_deref()?.trim();
        let message_id = event.discord_message_id.as_deref()?.trim();
        if channel_id.is_empty() || message_id.is_empty() || server.is_empty() {
            return None;
        }
        Some(logic::discord_message_link(&server, channel_id, message_id))
    })();
    let series = match event.series_id {
        Some(id) => match store::series(conn, id).await? {
            Some(series) => Some(series_dto(conn, series).await?),
            None => None,
        },
        None => None,
    };
    let creator = match event.created_by {
        Some(id) => store::membership(conn, id)
            .await?
            .map(|person| person.name)
            .filter(|name| !name.trim().is_empty()),
        None => None,
    }
    .unwrap_or_else(|| "Marian".into());
    let image_url = event
        .image_url
        .as_deref()
        .map(str::trim)
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(str::to_string);
    Ok(dto::Event {
        id: event.public_id.to_string(),
        title: event.title.clone(),
        description: event.description.clone(),
        location: event.location.trim().to_string(),
        status: event.status,
        starts_at: event.starts_at,
        duration_minutes: event.duration_minutes,
        cutoff_at: event.cutoff_at,
        post_at: event.post_at,
        color: event.color.clone(),
        image_url,
        channel_id: ids::public::channel(conn, event.channel_id).await?,
        channel: channel_name.clone(),
        attendee_role_id: ids::public::role(conn, event.attendee_role_id).await?,
        mention_everyone: event.mention_everyone,
        mention_role_ids: {
            let ids = store::event_role_ids(conn, &event, EventRoleKind::Mention).await?;
            ids::public::roles(conn, &ids).await?
        },
        restrict_role_ids: {
            let ids = store::event_role_ids(conn, &event, EventRoleKind::Restrict).await?;
            ids::public::roles(conn, &ids).await?
        },
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        signups_closed: event.signups_closed,
        series,
        creator,
        discord_url,
        google_url: ics::google_link(&event, &format!("#{channel_name}")).ok(),
        calendar_url: ics::calendar_template(&event).ok(),
        calendar_synced: event.calendar_synced_at.is_some(),
        google_synced: crate::google::on_calendar(conn, user.user_id, event.id).await?,
        can: dto::Abilities {
            edit: can_edit,
            delete: logic::can_delete(&user, &event, &settings, &roles),
            archive: can_edit && event.status == EventStatus::Posted,
            skip: can_edit
                && event.series_id.is_some()
                && matches!(event.status, EventStatus::Scheduled | EventStatus::Draft),
        },
    })
}

/// Someone who can edit the event adding, changing, or reordering its options.
pub async fn put_options(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    options: &[dto::SignupOption],
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let input = option_inputs(conn, viewer.guild_id, Some(event.id), options).await?;
    logic::save_options(conn, event.id, viewer.membership_id, &input).await?;
    Ok(Outcome::ok().synced(event.id))
}

pub async fn event_reminders(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Vec<dto::Reminder>, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    Ok(store::reminders_for(conn, &event)
        .await?
        .into_iter()
        .map(|rem| dto::Reminder {
            offset_minutes: rem.offset_minutes,
            message: rem.message,
            sent: rem.sent_at.is_some(),
        })
        .collect())
}

pub async fn signups(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<dto::Signups, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let settings = store::settings(conn, viewer.guild_id).await?;
    let user = must_member(conn, viewer).await?;
    let roles = store::membership_role_ids(conn, &user).await?;
    let can_manage = logic::can_edit(&user, &event, &settings, &roles);
    let options = store::options_for(conn, &event).await?;
    let signups = store::signups_for(conn, &event).await?;
    let people = store::list_memberships(conn, viewer.guild_id).await?;
    let role_rows = store::list_roles(conn, viewer.guild_id).await?;
    let restrict_ids = store::event_role_ids(conn, &event, EventRoleKind::Restrict).await?;
    let now = Utc::now();
    let closed =
        event.status != EventStatus::Posted || event.signups_closed || cutoff_passed(&event, now);
    let show_names = settings.show_attendees || can_manage;
    let named = |state: SignupState, option_id: i64| -> Vec<dto::Person> {
        if !show_names {
            return Vec::new();
        }
        signups
            .iter()
            .filter(|s| s.option_id == option_id && s.state == state)
            .filter_map(|s| {
                people
                    .iter()
                    .find(|p| p.id == s.membership_id)
                    .map(|p| (p, s.guests))
            })
            .map(|(p, guests)| dto::Person {
                id: p.public_id.to_string(),
                name: p.name.clone(),
                color: p.color.clone(),
                avatar: p.avatar.clone(),
                guests,
            })
            .collect()
    };
    let rows = options
        .iter()
        .map(|opt| {
            let confirmed = places(&signups, opt.id, SignupState::Confirmed);
            let my_signup = signups
                .iter()
                .find(|s| s.option_id == opt.id && s.membership_id == user.id);
            let mine = my_signup.is_some();
            let queue = places(&signups, opt.id, SignupState::Waitlisted) > 0;
            let gate = Gate {
                event: &event,
                user: &user,
                roles: &roles,
                restrict_ids: &restrict_ids,
                closed,
                waitlist: settings.waitlist,
            };
            let (open, reason, waitlist_next) = gate.button(opt, mine, confirmed, queue);
            let role = opt
                .role_id
                .and_then(|id| role_rows.iter().find(|r| r.id == id));
            dto::OptionSignups {
                id: opt.public_id.to_string(),
                type_id: opt.type_public_id.to_string(),
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                role_id: role.map(|r| r.public_id.to_string()),
                role: role.map(|r| r.name.clone()),
                attending: opt.reminders_enabled,
                max_guests: opt.max_guests,
                my_guests: my_signup.map(|s| s.guests).unwrap_or(0),
                confirmed_count: confirmed,
                waiting_count: places(&signups, opt.id, SignupState::Waitlisted),
                confirmed: named(SignupState::Confirmed, opt.id),
                waiting: named(SignupState::Waitlisted, opt.id),
                mine,
                open,
                reason,
                waitlist_next,
            }
        })
        .collect();
    Ok(dto::Signups {
        options: rows,
        notes: signup_notes(
            &event,
            &options,
            &signups,
            &user,
            &roles,
            &restrict_ids,
            &role_rows,
            settings.waitlist,
            now,
        ),
        show_names,
        can_manage,
        editable: can_manage
            && !matches!(
                event.status,
                EventStatus::Archived | EventStatus::Skipped | EventStatus::Cancelled
            ),
        posted: event.status == EventStatus::Posted,
    })
}

struct Gate<'a> {
    event: &'a store::Event,
    user: &'a store::Membership,
    roles: &'a [i64],
    restrict_ids: &'a [i64],
    closed: bool,
    /// The server lets a full option take signups onto a waitlist.
    waitlist: bool,
}

impl Gate<'_> {
    /// Whether the person can press this option's button, why not, and whether it waitlists.
    fn button(
        &self,
        opt: &store::SignupOption,
        mine: bool,
        confirmed: i64,
        queue: bool,
    ) -> (bool, String, bool) {
        if mine {
            return (
                self.event.status == EventStatus::Posted,
                String::new(),
                false,
            );
        }
        if self.event.status != EventStatus::Posted {
            return (
                false,
                "Signups open once the event is posted.".into(),
                false,
            );
        }
        if self.closed {
            return (false, "Signups are closed.".into(), false);
        }
        if !self.restrict_ids.is_empty()
            && !self.user.server_manager
            && !self.restrict_ids.iter().any(|id| self.roles.contains(id))
        {
            return (
                false,
                "This event is limited to another role.".into(),
                false,
            );
        }
        if let Some(role_id) = opt.role_id {
            if !self.user.server_manager && !self.roles.contains(&role_id) {
                return (false, "Your roles don't include this option.".into(), false);
            }
        }
        if let Some(cap) = opt.capacity {
            if confirmed >= cap || queue {
                if self.waitlist {
                    return (true, String::new(), true);
                }
                return (false, format!("{} is full.", opt.name), false);
            }
        }
        (true, String::new(), false)
    }
}

#[allow(clippy::too_many_arguments)]
fn signup_notes(
    event: &store::Event,
    options: &[store::SignupOption],
    signups: &[store::Signup],
    user: &store::Membership,
    roles: &[i64],
    restrict_ids: &[i64],
    role_rows: &[store::Role],
    waitlist: bool,
    now: DateTime<Utc>,
) -> Vec<String> {
    // A finished event has nothing left to say about signing up.
    if matches!(
        event.status,
        EventStatus::Archived | EventStatus::Skipped | EventStatus::Cancelled
    ) {
        return Vec::new();
    }
    let mut notes = Vec::new();
    if event.status != EventStatus::Posted {
        notes.push("Signups open once the event is posted.".to_string());
    }
    if event.signups_closed || cutoff_passed(event, now) {
        notes.push("Signups are closed.".into());
    }
    if !restrict_ids.is_empty()
        && !user.server_manager
        && !restrict_ids.iter().any(|id| roles.contains(id))
    {
        notes.push("This event is limited to another role.".into());
    }
    for opt in options {
        let yours = signups.iter().any(|s| {
            s.option_id == opt.id && s.membership_id == user.id && s.state == SignupState::Confirmed
        });
        if yours {
            continue;
        }
        if let Some(role_id) = opt.role_id {
            if !user.server_manager && !roles.contains(&role_id) {
                let role_name = role_rows
                    .iter()
                    .find(|role| role.id == role_id)
                    .map(|role| role.name.as_str())
                    .unwrap_or("another role");
                notes.push(format!("{} is limited to {role_name}.", opt.name));
            }
        }
        if let Some(cap) = opt.capacity {
            if places(signups, opt.id, SignupState::Confirmed) >= cap && !waitlist {
                notes.push(format!("{} is full.", opt.name));
            }
        }
    }
    if notes.len() > 1 {
        notes.retain(|note| note != "Signups open once the event is posted.");
    }
    notes
}

/// Places an option's signups in `state` take: each person and their guests.
fn places(signups: &[store::Signup], option_id: i64, state: SignupState) -> i64 {
    signups
        .iter()
        .filter(|s| s.option_id == option_id && s.state == state)
        .map(|s| 1 + s.guests)
        .sum()
}

fn cutoff_passed(event: &store::Event, now: DateTime<Utc>) -> bool {
    event.cutoff_at.is_some_and(|dt| now >= dt)
}

async fn signed_up(
    conn: &mut Connection,
    event: &store::Event,
    option_id: i64,
    membership_id: i64,
) -> Result<bool, OpError> {
    Ok(store::signups_for(conn, event)
        .await?
        .iter()
        .any(|s| s.option_id == option_id && s.membership_id == membership_id))
}

/// Signs the viewer up for an option, or takes them off it. With `guests`,
/// signs them up bringing that many, or changes the count when they're on it.
pub async fn set_my_signup(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    option: &str,
    on: bool,
    guests: Option<i64>,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let option_id = ids::public::option_id(conn, event.id, option).await?;
    if let Some(guests) = guests.filter(|_| on) {
        logic::signup_guests(conn, event.id, option_id, viewer.membership_id, guests).await?;
        return Ok(Outcome::ok().synced(event.id));
    }
    if signed_up(conn, &event, option_id, viewer.membership_id).await? == on {
        return Ok(Outcome::ok());
    }
    logic::signup(conn, event.id, option_id, viewer.membership_id).await?;
    Ok(Outcome::ok().synced(event.id))
}

/// Someone who can edit the event adding a person to an option, or taking them
/// off it. With `guests`, sets how many guests that person brings.
pub async fn set_signup(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    member: &str,
    option: &str,
    on: bool,
    guests: Option<i64>,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let member_id = ids::public::member_id(conn, viewer.guild_id, member).await?;
    let option_id = ids::public::option_id(conn, event.id, option).await?;
    logic::editor_response(
        conn,
        event.id,
        viewer.membership_id,
        member_id,
        option_id,
        if on { "add" } else { "remove" },
        guests,
    )
    .await?;
    Ok(Outcome::ok().synced(event.id))
}

pub async fn pending_reminders(
    conn: &mut Connection,
    viewer: &Viewer,
    limit: usize,
) -> Result<Vec<dto::PendingReminder>, OpError> {
    let sight = sight(conn, viewer).await?;
    let events: Vec<store::Event> = store::list_events(conn, viewer.guild_id)
        .await?
        .into_iter()
        .filter(|event| event.status == EventStatus::Posted && sight.sees_event(event))
        .collect();
    let groups = store::reminders_by_event(conn, &events).await?;
    Ok(events
        .iter()
        .zip(groups)
        .flat_map(|(event, rems)| {
            rems.into_iter()
                .filter(|rem| rem.sent_at.is_none())
                .map(|rem| dto::PendingReminder {
                    event_id: event.public_id.to_string(),
                    title: event.title.clone(),
                    offset_minutes: rem.offset_minutes,
                })
                .collect::<Vec<_>>()
        })
        .take(limit)
        .collect())
}

pub async fn activity(
    conn: &mut Connection,
    viewer: &Viewer,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    must_manager(conn, viewer).await?;
    activity_of(conn, viewer, None, page, limit).await
}

/// What happened to one event, newest first.
pub async fn event_activity(
    conn: &mut Connection,
    viewer: &Viewer,
    id: &str,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    let event = event_here(conn, viewer, id).await?;
    activity_of(conn, viewer, Some(event.id), page, limit).await
}

async fn activity_of(
    conn: &mut Connection,
    viewer: &Viewer,
    event_id: Option<i64>,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    let per_page = limit.clamp(1, 200) as u32;
    let sight = sight(conn, viewer).await?;
    let hidden: Vec<i64> = store::list_events(conn, viewer.guild_id)
        .await?
        .into_iter()
        .filter(|event| !sight.sees_event(event))
        .map(|event| event.id)
        .collect();
    let total = store::count_activity(conn, viewer.guild_id, event_id, &hidden).await? as usize;
    let pages = (total as u32).div_ceil(per_page).max(1);
    let page = page.clamp(1, pages);
    let offset = i64::from((page - 1) * per_page);
    let mut items = Vec::new();
    for item in store::list_activity(
        conn,
        viewer.guild_id,
        event_id,
        &hidden,
        i64::from(per_page),
        offset,
    )
    .await?
    {
        let event_id = match item.event_id {
            Some(id) => store::event(conn, id)
                .await?
                .map(|event| event.public_id.to_string()),
            None => None,
        };
        items.push(dto::Activity {
            body: item.body,
            created_at: item.created_at,
            event_id,
        });
    }
    Ok(dto::Paged {
        items,
        total,
        page,
        pages,
        per_page,
    })
}

fn local_instant(naive: NaiveDateTime, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    naive
        .and_local_timezone(tz)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .ok_or_else(|| OpError::message("That local time doesn't exist in this zone."))
}

fn end_of_day(date: NaiveDate, tz: Tz) -> Result<DateTime<Utc>, OpError> {
    clock::parse_date(&date.format("%Y-%m-%d").to_string(), tz)
}

async fn event_input(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::EventDraft,
    existing: Option<&store::Event>,
) -> Result<EventInput, OpError> {
    let tz = zone(conn, viewer.guild_id).await?;
    let in_series = existing.is_some_and(|event| event.series_id.is_some());
    let repeat = match &draft.repeat {
        Some(repeat) if !in_series && !repeat.cadence.is_empty() && repeat.cadence != "none" => {
            Some(RepeatSpec {
                cadence: RepeatSpec::cadence(&repeat.cadence)?,
                end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
                max_occurrences: repeat.count,
            })
        }
        _ => None,
    };
    let named: Vec<dto::SignupOption> = draft
        .options
        .iter()
        .filter(|opt| !opt.name.trim().is_empty())
        .take(8)
        .cloned()
        .collect();
    let options = option_inputs(
        conn,
        viewer.guild_id,
        existing.map(|event| event.id),
        &named,
    )
    .await?;
    Ok(EventInput {
        id: existing.map(|event| event.id),
        guild_id: viewer.guild_id,
        actor_id: viewer.membership_id,
        channel_id: channel_seen(conn, viewer, &draft.channel_id).await?,
        title: draft.title.clone(),
        description: draft.description.clone(),
        location: draft.location.clone(),
        starts_at: local_instant(draft.starts_local, tz)?,
        duration_minutes: draft.duration_minutes,
        color: draft.color.clone(),
        image_url: None,
        cutoff_at: draft
            .cutoff_local
            .map(|dt| local_instant(dt, tz))
            .transpose()?,
        attendee_role_id: ids::public::role_id(
            conn,
            viewer.guild_id,
            draft.attendee_role_id.as_deref(),
        )
        .await?,
        thread_enabled: draft.thread_enabled,
        multiple_signups: draft.multiple_signups,
        mention_everyone: draft.mention_everyone,
        mention_role_ids: ids::public::role_ids(conn, viewer.guild_id, &draft.mention_role_ids)
            .await?,
        restrict_role_ids: ids::public::role_ids(conn, viewer.guild_id, &draft.restrict_role_ids)
            .await?,
        options,
        reminders: draft
            .reminders
            .iter()
            .take(5)
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message.clone(),
            })
            .collect(),
        repeat,
        action: match draft.action {
            dto::SaveAction::Draft => logic::SaveAction::Draft,
            dto::SaveAction::Schedule => logic::SaveAction::Schedule,
            dto::SaveAction::Publish => logic::SaveAction::Publish,
            dto::SaveAction::Save => logic::SaveAction::Save,
        },
        post_at: draft
            .post_local
            .map(|dt| local_instant(dt, tz))
            .transpose()?,
        apply_series: draft.apply_series,
    })
}

async fn saved(conn: &mut Connection, viewer: &Viewer, event_id: i64) -> Result<Outcome, OpError> {
    let event = store::event(conn, event_id)
        .await?
        .ok_or(OpError::NotFound)?;
    let location = path(conn, viewer, &format!("/events/{}", event.public_id)).await?;
    Ok(Outcome::ok().at(location).synced(event_id))
}

pub async fn create_event(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::EventDraft,
) -> Result<Outcome, OpError> {
    let input = event_input(conn, viewer, draft, None).await?;
    let id = logic::save_event(conn, input).await?;
    saved(conn, viewer, id).await
}

pub async fn update_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::EventDraft,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let input = event_input(conn, viewer, draft, Some(&event)).await?;
    let id = logic::save_event(conn, input).await?;
    saved(conn, viewer, id).await
}

/// Changes a few details of an event and keeps the rest, including its status.
/// Moving the start moves a set cutoff with it.
/// Taking an event that hasn't gone out to Discord yet: now, or at a time.
enum Posting {
    Now,
    At(NaiveDateTime),
    /// Back to a draft that doesn't post on its own.
    Hold,
}

pub async fn patch_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    patch: &dto::EventPatch,
) -> Result<Outcome, OpError> {
    save_stored(conn, viewer, public_id, patch, None).await
}

/// Posts a draft or a scheduled event to Discord right away.
pub async fn publish_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    save_stored(
        conn,
        viewer,
        public_id,
        &dto::EventPatch::default(),
        Some(Posting::Now),
    )
    .await
}

/// Takes a scheduled or failed post back to a draft.
pub async fn unschedule_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    save_stored(
        conn,
        viewer,
        public_id,
        &dto::EventPatch::default(),
        Some(Posting::Hold),
    )
    .await
}

/// Sets when Marian posts a draft, or moves a scheduled post.
pub async fn schedule_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    when: &dto::PostAt,
) -> Result<Outcome, OpError> {
    save_stored(
        conn,
        viewer,
        public_id,
        &dto::EventPatch::default(),
        Some(Posting::At(when.post_local)),
    )
    .await
}

/// Saves an event from what is stored, with `patch` on top and, when it has
/// not gone out yet, a change of when it posts. Everything else stays as it is.
async fn save_stored(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    patch: &dto::EventPatch,
    posting: Option<Posting>,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    if posting.is_some()
        && !matches!(
            event.status,
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
        )
    {
        return Err(OpError::message("This event has already gone out."));
    }
    let unposted = matches!(
        event.status,
        EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
    ) && event.discord_message_id.is_none();
    let channel_id = match &patch.channel_id {
        Some(public) => channel_seen(conn, viewer, public).await?,
        None => event.channel_id,
    };
    if channel_id != event.channel_id && !unposted {
        return Err(OpError::message("A posted event stays in its channel."));
    }
    if patch.apply_series && event.series_id.is_none() {
        return Err(OpError::message("This event isn't part of a series."));
    }
    if patch.repeat.is_some() && event.series_id.is_some() {
        return Err(OpError::message(
            "This date already belongs to a series. Change the repeat there.",
        ));
    }
    let tz = zone(conn, viewer.guild_id).await?;
    let starts_at = match patch.starts_local {
        Some(local) => local_instant(local, tz)?,
        None => event.starts_at,
    };
    let cutoff_at = match patch.cutoff {
        Some(dto::Cutoff::Never) => None,
        Some(dto::Cutoff::Before { minutes }) if minutes >= 0 => {
            Some(starts_at - Duration::minutes(minutes))
        }
        Some(dto::Cutoff::Before { .. }) => {
            return Err(OpError::message(
                "Signups have to close before the event starts.",
            ));
        }
        None => event
            .cutoff_at
            .map(|cutoff| cutoff + (starts_at - event.starts_at)),
    };
    let options = store::options_for(conn, &event)
        .await?
        .into_iter()
        .map(|opt| OptionInput {
            id: Some(opt.id),
            type_id: opt.type_id,
            capacity: opt.capacity,
            max_guests: opt.max_guests,
            role_id: opt.role_id,
        })
        .collect();
    let reminders = match &patch.reminders {
        Some(list) => list
            .iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message.clone(),
            })
            .collect(),
        None => store::reminders_for(conn, &event)
            .await?
            .into_iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message,
            })
            .collect(),
    };
    let mention_role_ids = match &patch.mention_role_ids {
        Some(public) => ids::public::role_ids(conn, viewer.guild_id, public).await?,
        None => store::event_role_ids(conn, &event, EventRoleKind::Mention).await?,
    };
    let restrict_role_ids = match &patch.restrict_role_ids {
        Some(public) => ids::public::role_ids(conn, viewer.guild_id, public).await?,
        None => store::event_role_ids(conn, &event, EventRoleKind::Restrict).await?,
    };
    let attendee_role_id = match &patch.attendee_role_id {
        Some(public) => ids::public::role_id(conn, viewer.guild_id, public.as_deref()).await?,
        None => event.attendee_role_id,
    };
    let input = EventInput {
        id: Some(event.id),
        guild_id: viewer.guild_id,
        actor_id: viewer.membership_id,
        channel_id,
        title: patch.title.clone().unwrap_or_else(|| event.title.clone()),
        description: patch
            .description
            .clone()
            .unwrap_or_else(|| event.description.clone()),
        location: patch
            .location
            .clone()
            .unwrap_or_else(|| event.location.clone()),
        starts_at,
        duration_minutes: patch.duration_minutes.unwrap_or(event.duration_minutes),
        color: patch.color.clone().unwrap_or_else(|| event.color.clone()),
        image_url: event.image_url.clone(),
        cutoff_at,
        attendee_role_id,
        thread_enabled: patch.thread_enabled.unwrap_or(event.thread_enabled),
        multiple_signups: patch.multiple_signups.unwrap_or(event.multiple_signups),
        mention_everyone: patch.mention_everyone.unwrap_or(event.mention_everyone),
        mention_role_ids,
        restrict_role_ids,
        options,
        reminders,
        repeat: match &patch.repeat {
            Some(repeat) => Some(RepeatSpec {
                cadence: RepeatSpec::cadence(&repeat.cadence)?,
                end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
                max_occurrences: repeat.count,
            }),
            None => None,
        },
        action: match &posting {
            Some(Posting::Now) => logic::SaveAction::Publish,
            Some(Posting::At(_)) => logic::SaveAction::Schedule,
            Some(Posting::Hold) => logic::SaveAction::Draft,
            None if event.status == EventStatus::Draft => logic::SaveAction::Draft,
            None => logic::SaveAction::Save,
        },
        post_at: match posting {
            Some(Posting::At(local)) => Some(local_instant(local, tz)?),
            _ => event.post_at,
        },
        apply_series: patch.apply_series,
    };
    logic::save_event(conn, input).await?;
    Ok(Outcome::ok().synced(event.id))
}

pub async fn delete_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    logic::delete_event(conn, event.id, viewer.membership_id).await?;
    // A date of a series stays behind as skipped, so only a removed event leaves its page.
    let gone = store::event(conn, event.id).await?.is_none();
    let location = if gone {
        path(conn, viewer, "").await?
    } else {
        path(conn, viewer, &format!("/events/{public_id}")).await?
    };
    Ok(Outcome::ok().at(location).synced(event.id))
}

pub async fn archive_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    logic::archive_event(conn, event.id, viewer.membership_id).await?;
    Ok(Outcome::ok().synced(event.id))
}

pub async fn duplicate_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let copy = logic::duplicate_event(conn, event.id, viewer.membership_id).await?;
    let copy = store::event(conn, copy).await?.ok_or(OpError::NotFound)?;
    let location = path(conn, viewer, &format!("/events/{}", copy.public_id)).await?;
    Ok(Outcome::ok().at(location))
}

pub async fn skip_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    logic::skip_event(conn, event.id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

async fn series_dto(conn: &mut Connection, series: store::Series) -> Result<dto::Series, OpError> {
    let template = logic::series_template(conn, &series).await?;
    let events = store::events_in_series(conn, series.id).await?;
    let next = events
        .iter()
        .find(|event| {
            matches!(
                event.status,
                EventStatus::Posted | EventStatus::Scheduled | EventStatus::Draft
            )
        })
        .map(|event| event.starts_at);
    Ok(dto::Series {
        id: series.public_id.to_string(),
        title: template.title,
        cadence: series.cadence,
        active: series.active,
        max_occurrences: series.max_occurrences,
        end_on: series.end_on,
        next,
        dates: events.len() as i64,
    })
}

pub async fn series_list(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<Vec<dto::Series>, OpError> {
    must_manager(conn, viewer).await?;
    let sight = sight(conn, viewer).await?;
    let mut rows = Vec::new();
    for series in store::list_series(conn, viewer.guild_id).await? {
        if series.cadence != Cadence::Once
            && sight.sees_channel(template_channel(conn, series.template_id).await?)
        {
            rows.push(series_dto(conn, series).await?);
        }
    }
    Ok(rows)
}

/// One series, for a manager or someone who can edit one of its dates.
pub async fn series(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<dto::Series, OpError> {
    let series = series_here(conn, viewer, public_id).await?;
    let user = must_member(conn, viewer).await?;
    if !user.server_manager {
        let settings = store::settings(conn, viewer.guild_id).await?;
        let roles = store::membership_role_ids(conn, &user).await?;
        let editable = store::events_in_series(conn, series.id)
            .await?
            .iter()
            .any(|event| logic::can_edit(&user, event, &settings, &roles));
        if !editable {
            return Err(OpError::NotFound);
        }
    }
    series_dto(conn, series).await
}

pub async fn patch_series(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    repeat: &dto::Repeat,
) -> Result<Outcome, OpError> {
    let series = series_here(conn, viewer, public_id).await?;
    let tz = zone(conn, viewer.guild_id).await?;
    logic::save_series_repeat(
        conn,
        series.id,
        viewer.membership_id,
        RepeatSpec {
            cadence: RepeatSpec::cadence(&repeat.cadence)?,
            end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
            max_occurrences: repeat.count,
        },
    )
    .await?;
    Ok(Outcome::ok())
}

pub async fn end_series(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let series = series_here(conn, viewer, public_id).await?;
    logic::end_series(conn, series.id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

/// Options from a request, with their public ids turned into keys. An id that
/// isn't one of this event's options counts as a new option; a type has to be
/// in this server's library.
async fn option_inputs(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    options: &[dto::SignupOption],
) -> Result<Vec<OptionInput>, OpError> {
    let known = match event_id {
        Some(event_id) => ids::public::option_ids(conn, event_id).await?,
        None => Default::default(),
    };
    let types: HashMap<String, i64> = store::list_library(conn, guild_id)
        .await?
        .into_iter()
        .map(|kind| (kind.public_id.to_string(), kind.id))
        .collect();
    let mut inputs = Vec::with_capacity(options.len());
    for opt in options {
        let type_id = *types.get(opt.type_id.trim()).ok_or_else(|| {
            OpError::message("That attendance type isn't in this server's library.")
        })?;
        inputs.push(OptionInput {
            id: opt.id.as_ref().and_then(|id| known.get(id).copied()),
            type_id,
            capacity: opt.capacity,
            max_guests: opt.max_guests,
            role_id: ids::public::role_id(conn, guild_id, opt.role_id.as_deref()).await?,
        });
    }
    Ok(inputs)
}

/// A preset's or template's type as the page shows it: the library type's name,
/// emoji, and attending, with this use's capacity and role.
async fn option_dto(
    conn: &mut Connection,
    library: &HashMap<i64, store::LibraryOption>,
    opt: &OptionInput,
) -> Result<dto::SignupOption, OpError> {
    let kind = library.get(&opt.type_id).ok_or(OpError::NotFound)?;
    Ok(dto::SignupOption {
        id: None,
        type_id: kind.public_id.to_string(),
        name: kind.name.clone(),
        emoji: kind.emoji.clone(),
        attending: kind.attending,
        capacity: opt.capacity,
        max_guests: opt.max_guests,
        role_id: ids::public::role(conn, opt.role_id).await?,
    })
}

async fn library_by_id(
    conn: &mut Connection,
    guild_id: i64,
) -> Result<HashMap<i64, store::LibraryOption>, OpError> {
    Ok(store::list_library(conn, guild_id)
        .await?
        .into_iter()
        .map(|kind| (kind.id, kind))
        .collect())
}

pub async fn presets(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Preset>, OpError> {
    let library = library_by_id(conn, viewer.guild_id).await?;
    let mut presets = Vec::new();
    for (preset, types, default) in logic::presets(conn, viewer.guild_id).await? {
        let mut options = Vec::with_capacity(types.len());
        for opt in &types {
            options.push(option_dto(conn, &library, opt).await?);
        }
        presets.push(dto::Preset {
            id: preset.public_id.to_string(),
            name: preset.name,
            multiple_signups: preset.multiple_signups,
            default,
            options,
        });
    }
    Ok(presets)
}

pub async fn preset(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<dto::Preset, OpError> {
    let preset = preset_here(conn, viewer, public_id).await?;
    presets(conn, viewer)
        .await?
        .into_iter()
        .find(|shown| shown.id == preset.public_id.to_string())
        .ok_or(OpError::NotFound)
}

async fn preset_here(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<store::Preset, OpError> {
    store::preset_by_public(conn, public_id)
        .await?
        .filter(|preset| preset.guild_id == viewer.guild_id)
        .ok_or(OpError::NotFound)
}

async fn preset_input(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::PresetDraft,
) -> Result<logic::PresetInput, OpError> {
    Ok(logic::PresetInput {
        name: draft.name.clone(),
        multiple_signups: draft.multiple_signups,
        options: option_inputs(conn, viewer.guild_id, None, &draft.options).await?,
    })
}

pub async fn create_preset(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::PresetDraft,
) -> Result<Outcome, OpError> {
    let input = preset_input(conn, viewer, draft).await?;
    logic::save_preset(conn, viewer.guild_id, viewer.membership_id, None, &input).await?;
    Ok(Outcome::ok())
}

pub async fn update_preset(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::PresetDraft,
) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id).await?;
    let input = preset_input(conn, viewer, draft).await?;
    logic::save_preset(
        conn,
        viewer.guild_id,
        viewer.membership_id,
        Some(preset.id),
        &input,
    )
    .await?;
    Ok(Outcome::ok())
}

pub async fn default_preset(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id).await?;
    logic::set_default_preset(conn, preset.id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

pub async fn delete_preset(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id).await?;
    logic::delete_preset(conn, preset.id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

/// Gives an event a preset's attendance; responses on types it keeps stay.
pub async fn apply_preset(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    body: &dto::ApplyPreset,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id).await?;
    let preset = preset_here(conn, viewer, &body.preset).await?;
    logic::apply_preset(conn, event.id, viewer.membership_id, preset.id).await?;
    Ok(Outcome::ok().synced(event.id))
}

pub async fn library(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<Vec<dto::LibraryItem>, OpError> {
    let usage = store::type_usage(conn, viewer.guild_id).await?;
    Ok(store::list_library(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|item| dto::LibraryItem {
            used_by: usage
                .get(&item.id)
                .filter(|used| used.total() > 0)
                .map(|used| used.describe())
                .unwrap_or_default(),
            id: item.public_id.to_string(),
            name: item.name,
            emoji: item.emoji,
            attending: item.attending,
        })
        .collect())
}

fn type_input(draft: &dto::TypeDraft) -> logic::TypeInput {
    logic::TypeInput {
        name: draft.name.clone(),
        emoji: draft.emoji.clone(),
        attending: draft.attending,
    }
}

/// Adds a type to the library and returns it, so a page can use it at once.
pub async fn add_library(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::TypeDraft,
) -> Result<dto::LibraryItem, OpError> {
    let made = logic::add_type(
        conn,
        viewer.guild_id,
        viewer.membership_id,
        &type_input(draft),
    )
    .await?;
    Ok(dto::LibraryItem {
        id: made.public_id.to_string(),
        name: made.name,
        emoji: made.emoji,
        attending: made.attending,
        used_by: String::new(),
    })
}

async fn type_here(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<store::LibraryOption, OpError> {
    store::library_type_by_public(conn, viewer.guild_id, public_id)
        .await?
        .ok_or(OpError::NotFound)
}

/// Renames a type, or changes its emoji or attending, everywhere it is used.
/// Posted events that use it are refreshed on Discord.
pub async fn update_library(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::TypeDraft,
) -> Result<Outcome, OpError> {
    let kind = type_here(conn, viewer, public_id).await?;
    let posted =
        logic::update_type(conn, kind.id, viewer.membership_id, &type_input(draft)).await?;
    Ok(Outcome {
        jobs: posted.into_iter().map(DiscordJob::sync).collect(),
        ..Outcome::ok()
    })
}

pub async fn delete_library(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let kind = type_here(conn, viewer, public_id).await?;
    logic::delete_type(conn, kind.id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

pub async fn routines(
    conn: &mut Connection,
    viewer: &Viewer,
) -> Result<Vec<dto::RoutineSummary>, OpError> {
    use diesel::prelude::*;
    use diesel_async::RunQueryDsl;
    must_manager(conn, viewer).await?;
    let hidden: Vec<String> = crate::schema::routines::table
        .inner_join(crate::schema::event_templates::table)
        .filter(crate::schema::routines::guild_id.eq(viewer.guild_id))
        .filter(
            crate::schema::event_templates::channel_id
                .eq_any(sight(conn, viewer).await?.hidden_channels()),
        )
        .select(crate::schema::routines::public_id)
        .load::<uuid::Uuid>(conn)
        .await?
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    Ok(logic::list_routines(conn, viewer.guild_id)
        .await?
        .into_iter()
        .filter(|routine| !hidden.contains(&routine.id))
        .collect())
}

/// One routine, for a manager, its creator, or someone who can answer its
/// waiting approval, which Discord links here.
pub async fn routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<dto::RoutineView, OpError> {
    use diesel::prelude::*;
    use diesel_async::RunQueryDsl;
    let id = routine_here(conn, viewer, public_id).await?;
    let can_decide = logic::can_decide(conn, id, viewer.membership_id).await?;
    if !must_member(conn, viewer).await?.server_manager && !can_decide {
        let creator: Option<i64> = crate::schema::routines::table
            .find(id)
            .select(crate::schema::routines::created_by)
            .first(conn)
            .await?;
        if creator != Some(viewer.membership_id) {
            return Err(OpError::NotFound);
        }
    }
    let mut view = logic::routine_page_data(conn, id)
        .await?
        .ok_or(OpError::NotFound)?;
    if let Some(approval) = view.approval.as_mut() {
        approval.can_decide = can_decide;
    }
    Ok(view)
}

/// A routine form's steps, with public ids turned into keys.
async fn routine_steps(
    conn: &mut Connection,
    viewer: &Viewer,
    steps: &[dto::RoutineStep],
) -> Result<Vec<logic::Step>, OpError> {
    let roles: std::collections::HashMap<String, i64> = store::list_roles(conn, viewer.guild_id)
        .await?
        .into_iter()
        .map(|role| (role.public_id.to_string(), role.id))
        .collect();
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let kind = step
            .kind
            .parse::<logic::StepKind>()
            .map_err(|_| OpError::message("That step isn't one a routine has."))?;
        let channel_id = match step.channel_id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => Some(channel_seen(conn, viewer, id).await?),
            None => None,
        };
        out.push(logic::Step {
            kind,
            channel_id,
            role_ids: step
                .role_ids
                .iter()
                .filter_map(|id| roles.get(id).copied())
                .collect(),
        });
    }
    Ok(out)
}

/// A routine form's channel and event details, with public ids turned into keys.
async fn routine_parts(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::RoutineDraft,
) -> Result<(i64, Option<logic::EventExtras>), OpError> {
    let channel_id = channel_seen(conn, viewer, &draft.channel_id).await?;
    let extras = match &draft.event {
        Some(event) => Some(logic::routine_event_from(conn, viewer.guild_id, event).await?),
        None => None,
    };
    Ok((channel_id, extras))
}

fn clock_minutes(raw: &str) -> Result<u16, OpError> {
    let pick = || OpError::message("Pick a time.");
    let (hour, minute) = raw.trim().split_once(':').ok_or_else(pick)?;
    let hour: u16 = hour.parse().map_err(|_| pick())?;
    let minute: u16 = minute.parse().map_err(|_| pick())?;
    if hour > 23 || minute > 59 {
        return Err(pick());
    }
    Ok(hour * 60 + minute)
}

fn weekday(day: u8) -> Result<u8, OpError> {
    if day < 7 {
        Ok(day)
    } else {
        Err(OpError::message("Pick a weekday."))
    }
}

pub async fn create_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::RoutineDraft,
) -> Result<Outcome, OpError> {
    let (channel_id, extras) = routine_parts(conn, viewer, draft).await?;
    let steps = match &draft.steps {
        Some(steps) => {
            let steps = routine_steps(conn, viewer, steps).await?;
            logic::check_steps(conn, viewer.guild_id, &steps).await?;
            Some(steps)
        }
        None => None,
    };
    let (id, public_id, jobs) = logic::create_routine(
        conn,
        viewer.guild_id,
        viewer.membership_id,
        channel_id,
        &draft.title,
        &draft.description,
        draft.duration_minutes,
        clock_minutes(&draft.start_time)?,
        weekday(draft.poll_weekday)?,
        clock_minutes(&draft.poll_time)?,
        weekday(draft.lock_weekday)?,
        clock_minutes(&draft.lock_time)?,
        &draft.days,
        extras,
        Utc::now(),
    )
    .await?;
    if let Some(steps) = steps {
        logic::save_steps(conn, id, viewer.membership_id, &steps).await?;
    }
    let location = path(conn, viewer, &format!("/routines/{public_id}")).await?;
    Ok(Outcome {
        jobs,
        ..Outcome::ok().at(location)
    })
}

pub async fn update_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::RoutineDraft,
) -> Result<Outcome, OpError> {
    let id = routine_here(conn, viewer, public_id).await?;
    let (channel_id, extras) = routine_parts(conn, viewer, draft).await?;
    let effect = logic::save_routine(
        conn,
        id,
        viewer.membership_id,
        channel_id,
        &draft.title,
        &draft.description,
        draft.duration_minutes,
        clock_minutes(&draft.start_time)?,
        weekday(draft.poll_weekday)?,
        clock_minutes(&draft.poll_time)?,
        weekday(draft.lock_weekday)?,
        clock_minutes(&draft.lock_time)?,
        &draft.days,
        extras,
        Utc::now(),
    )
    .await?;
    if let Some(steps) = &draft.steps {
        let steps = routine_steps(conn, viewer, steps).await?;
        logic::save_steps(conn, id, viewer.membership_id, &steps).await?;
    }
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::ok()
    })
}

/// Answers the approval a routine's closed poll waits on.
pub async fn decide_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    decision: &dto::Decision,
) -> Result<Outcome, OpError> {
    let id = routine_here(conn, viewer, public_id).await?;
    let run = logic::waiting_run_id(conn, id)
        .await?
        .ok_or_else(|| OpError::from(crate::msg!("err-approval-closed")))?;
    let effect =
        logic::decide_run(conn, run, viewer.membership_id, decision.day, Utc::now()).await?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::ok()
    })
}

pub async fn routine_days(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    days: &dto::RoutineDays,
) -> Result<Outcome, OpError> {
    let id = routine_here(conn, viewer, public_id).await?;
    match days.scope {
        dto::DayScope::Usual => {
            logic::save_usual_days(conn, id, viewer.membership_id, &days.days).await?;
            Ok(Outcome::ok())
        }
        dto::DayScope::Run => {
            let effect = logic::save_run_days(conn, id, viewer.membership_id, &days.days).await?;
            Ok(Outcome {
                jobs: effect.jobs,
                ..Outcome::ok()
            })
        }
    }
}

pub async fn vote(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    vote: &dto::Vote,
) -> Result<Outcome, OpError> {
    let id = routine_here(conn, viewer, public_id).await?;
    let run = logic::open_run_id(conn, id)
        .await?
        .ok_or_else(|| OpError::message("That poll is closed."))?;
    let effect = logic::cast_run(conn, run, viewer.membership_id, vote.day).await?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::ok()
    })
}

pub async fn stop_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Outcome, OpError> {
    let id = routine_here(conn, viewer, public_id).await?;
    logic::stop_routine(conn, id, viewer.membership_id).await?;
    Ok(Outcome::ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::{ExpressionMethods, QueryDsl};
    use diesel_async::RunQueryDsl;
    use diesel_async::SimpleAsyncConnection;

    // Library types the test server has.
    const YES: i64 = 201;
    const TANK: i64 = 202;
    const MAYBE: i64 = 203;

    async fn world() -> (Connection, Viewer) {
        let mut conn = store::open_memory().await.unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Test', 'Europe/Warsaw', '99');
            INSERT INTO settings (guild_id) VALUES (1);
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'events', 'text', 0);
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (2, 1, 'elsewhere', 'text', 1);
            INSERT INTO roles (id, guild_id, name, color) VALUES (1, 1, 'Raider', '#888888');
            INSERT INTO option_library (id, guild_id, name, emoji, attending) VALUES
              (201, 1, 'Yes', '✅', true), (202, 1, 'Tank', '🛡', true), (203, 1, 'Maybe', '•', false);
            INSERT INTO users (discord_id) VALUES ('u1'), ('u2');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'u1'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#223344', true, false FROM users WHERE discord_id = 'u2';
            ",
        )
        .await.unwrap();
        let viewer = Viewer {
            token: String::new(),
            discord_user_id: "u1".into(),
            user_id: 1,
            display_name: "Nell".into(),
            guild_id: 1,
            membership_id: 1,
            allowed: vec![1],
            notice: None,
        };
        (conn, viewer)
    }

    /// Makes every `op` on `table` fail, as a lost connection or a full disk would.
    async fn refuse(conn: &mut Connection, op: &str, table: &str) {
        conn.batch_execute(&format!(
            "CREATE OR REPLACE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS
               $$ BEGIN RAISE EXCEPTION 'refused'; END $$;
             CREATE TRIGGER refuse BEFORE {op} ON {table} FOR EACH ROW EXECUTE FUNCTION refuse();"
        ))
        .await
        .unwrap();
    }

    /// Every id in an API answer is a UUIDv7 string; a row's sequential key never leaves.
    fn assert_public_ids(value: &serde_json::Value, path: &str) {
        let id_key = |key: &str| {
            key == "id" || key.ends_with("_id") || key.ends_with("_ids") || key.starts_with("role_")
        };
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    let here = format!("{path}.{key}");
                    if id_key(key) {
                        let ids: Vec<&serde_json::Value> = match inner {
                            serde_json::Value::Array(list) => list.iter().collect(),
                            other => vec![other],
                        };
                        for id in ids.into_iter().filter(|id| !id.is_null()) {
                            let text = id
                                .as_str()
                                .unwrap_or_else(|| panic!("{here} is {id}, not a public id"));
                            let parsed = uuid::Uuid::parse_str(text)
                                .unwrap_or_else(|_| panic!("{here} is {text:?}"));
                            assert_eq!(parsed.get_version_num(), 7, "{here} is not a UUIDv7");
                        }
                    } else {
                        assert_public_ids(inner, &here);
                    }
                }
            }
            serde_json::Value::Array(list) => {
                for (index, inner) in list.iter().enumerate() {
                    assert_public_ids(inner, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn the_api_names_rows_only_by_public_id() {
        let (mut conn, viewer) = world().await;
        let id = logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: String::new(),
                location: String::new(),
                starts_at: Utc::now() + Duration::days(2),
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: None,
                attendee_role_id: Some(1),
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![1],
                restrict_role_ids: vec![1],
                options: vec![OptionInput {
                    capacity: Some(2),
                    role_id: Some(1),
                    ..OptionInput::of_type(TANK)
                }],
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .await
        .unwrap();
        let public = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .public_id
            .to_string();
        let option = signups(&mut conn, &viewer, &public).await.unwrap().options[0]
            .id
            .clone();
        let june = members(&mut conn, &viewer)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.name == "June")
            .unwrap()
            .id;
        set_signup(&mut conn, &viewer, &public, &june, &option, true, None)
            .await
            .unwrap();
        let mut next = settings(&mut conn, &viewer).await.unwrap();
        next.role_event = roles(&mut conn, &viewer)
            .await
            .unwrap()
            .into_iter()
            .map(|role| role.id)
            .collect();
        put_settings(&mut conn, &viewer, &next).await.unwrap();
        let routine_id = approved_routine(&mut conn, &viewer).await;

        let answers = [
            (
                "event",
                serde_json::to_value(event(&mut conn, &viewer, &public).await.unwrap()).unwrap(),
            ),
            (
                "signups",
                serde_json::to_value(signups(&mut conn, &viewer, &public).await.unwrap()).unwrap(),
            ),
            (
                "settings",
                serde_json::to_value(settings(&mut conn, &viewer).await.unwrap()).unwrap(),
            ),
            (
                "roles",
                serde_json::to_value(roles(&mut conn, &viewer).await.unwrap()).unwrap(),
            ),
            (
                "members",
                serde_json::to_value(members(&mut conn, &viewer).await.unwrap()).unwrap(),
            ),
            (
                "channels",
                serde_json::to_value(channels(&mut conn, &viewer).await.unwrap()).unwrap(),
            ),
            (
                "membership",
                serde_json::to_value(membership(&mut conn, &viewer).await.unwrap()).unwrap(),
            ),
            (
                "routine",
                serde_json::to_value(routine(&mut conn, &viewer, &routine_id).await.unwrap())
                    .unwrap(),
            ),
        ];
        for (name, answer) in &answers {
            assert_public_ids(answer, name);
        }
        let shown = answers[1].1.to_string();
        assert!(shown.contains(&june), "June shows by her public id");
        assert_eq!(
            answers[2].1["role_event"].as_array().unwrap().len(),
            1,
            "settings keep the role"
        );
    }

    #[test]
    fn the_calendar_feed_is_a_full_address_when_the_server_knows_its_own() {
        assert_eq!(
            calendar_feed_url("https://marian.example/ ", "abc"),
            "https://marian.example/calendar/abc.ics"
        );
        assert_eq!(
            calendar_feed_url("", "abc"),
            "/calendar/abc.ics",
            "else a path the browser completes"
        );
    }

    #[tokio::test]
    async fn membership_says_who_can_create_events() {
        let (mut conn, nell) = world().await;
        let june = Viewer {
            membership_id: 2,
            discord_user_id: "u2".into(),
            user_id: 2,
            display_name: "June".into(),
            ..nell.clone()
        };
        assert!(
            membership(&mut conn, &june).await.unwrap().can_create_event,
            "no event roles leaves creating open"
        );
        let mut next = settings(&mut conn, &nell).await.unwrap();
        next.role_event = roles(&mut conn, &nell)
            .await
            .unwrap()
            .into_iter()
            .map(|role| role.id)
            .collect();
        put_settings(&mut conn, &nell, &next).await.unwrap();
        assert!(
            !membership(&mut conn, &june).await.unwrap().can_create_event,
            "June lacks the role"
        );
        assert!(
            membership(&mut conn, &nell).await.unwrap().can_create_event,
            "Nell manages the server"
        );
        conn.batch_execute("INSERT INTO membership_roles (membership_id, role_id) VALUES (2, 1);")
            .await
            .unwrap();
        assert!(
            membership(&mut conn, &june).await.unwrap().can_create_event,
            "June holds the role now"
        );
    }

    #[tokio::test]
    async fn an_id_from_another_server_is_not_found() {
        let (mut conn, viewer) = world().await;
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (2, 'Other', 'UTC', '77');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (9, 2, 'theirs', 'text', 0);
            INSERT INTO roles (id, guild_id, name, color) VALUES (9, 2, 'Theirs', '#888888');
            INSERT INTO users (discord_id) VALUES ('u9');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 9, id, 2, 'Kit', 'kit', '#112233', true, false FROM users WHERE discord_id = 'u9';
            ",
        )
        .await.unwrap();
        let their_role = ids::public::roles(&mut conn, &[9]).await.unwrap().remove(0);
        let their_channel = ids::public::channel(&mut conn, 9).await.unwrap();
        assert!(
            ids::public::role_ids(&mut conn, 1, &[their_role])
                .await
                .is_err()
        );
        assert!(
            ids::public::channel_id(&mut conn, 1, &their_channel)
                .await
                .is_err()
        );
        let their_member: String = crate::schema::memberships::table
            .find(9)
            .select(crate::schema::memberships::public_id)
            .first::<uuid::Uuid>(&mut conn)
            .await
            .unwrap()
            .to_string();
        assert!(matches!(
            ids::public::member_id(&mut conn, viewer.guild_id, &their_member).await,
            Err(OpError::NotFound)
        ));
        assert!(
            matches!(
                ids::public::member_id(&mut conn, viewer.guild_id, "9").await,
                Err(OpError::NotFound)
            ),
            "a row key is not an id here"
        );
    }

    #[tokio::test]
    async fn a_server_you_cannot_open_reads_as_one_that_does_not_exist() {
        let (mut conn, viewer) = world().await;
        conn.batch_execute(
            "INSERT INTO guilds (id, name, timezone, discord_id) VALUES (2, 'Other', 'UTC', '77');",
        )
        .await
        .unwrap();
        let ask = async |conn: &mut Connection, server: &str| {
            let mut viewer = viewer.clone();
            bind_server(conn, &mut viewer, server).await
        };
        assert!(
            matches!(ask(&mut conn, "77").await, Err(OpError::NotFound)),
            "there, but not theirs"
        );
        assert!(
            matches!(ask(&mut conn, "78").await, Err(OpError::NotFound)),
            "not there at all"
        );
        assert!(ask(&mut conn, "99").await.is_ok(), "their own");
    }

    #[tokio::test]
    async fn an_event_lists_only_its_own_activity_newest_first() {
        let (mut conn, viewer) = world().await;
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (2, 'Other', 'UTC', '98');
            INSERT INTO events (id, public_id, guild_id, channel_id, title, starts_at, status, created_at, updated_at)
            VALUES (11, marian_uuid_v7(), 1, 1, 'Raid', now(), 'draft', now(), now()),
                   (12, marian_uuid_v7(), 1, 1, 'Quiz', now(), 'draft', now(), now()),
                   (13, marian_uuid_v7(), 2, 1, 'Theirs', now(), 'draft', now(), now());
            INSERT INTO activity (guild_id, event_id, body, created_at) VALUES
              (1, 11, 'Nell created Raid', now() - interval '3 minutes'),
              (1, 12, 'Nell created Quiz', now() - interval '2 minutes'),
              (1, NULL, 'Settings changed', now() - interval '90 seconds'),
              (1, 11, 'June signed up for Raid', now() - interval '1 minute');
            ",
        )
        .await.unwrap();
        let public = async |conn: &mut Connection, id: i64| {
            store::event(conn, id)
                .await
                .unwrap()
                .unwrap()
                .public_id
                .to_string()
        };
        let raid = public(&mut conn, 11).await;
        let bodies: Vec<String> = event_activity(&mut conn, &viewer, &raid, 1, 25)
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.body)
            .collect();
        assert_eq!(bodies, ["June signed up for Raid", "Nell created Raid"]);
        assert_eq!(
            activity(&mut conn, &viewer, 1, 25).await.unwrap().total,
            4,
            "the server's log keeps everything"
        );
        let theirs = public(&mut conn, 13).await;
        assert!(matches!(
            event_activity(&mut conn, &viewer, &theirs, 1, 25).await,
            Err(OpError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_member_sees_only_the_events_in_channels_discord_shows_them() {
        let (mut conn, nell) = world().await;
        // #elsewhere is closed to @everyone (the server's id, 99) and open to Raider.
        conn.batch_execute(
            "
            UPDATE guilds SET everyone_permissions = 1024 WHERE id = 1;
            UPDATE channels SET discord_id = 'c' || id WHERE guild_id = 1;
            UPDATE roles SET discord_id = 'r1' WHERE id = 1;
            INSERT INTO channel_overwrites (channel_id, kind, target, allow, deny)
            VALUES (2, 'role', '99', 0, 1024), (2, 'role', 'r1', 1024, 0);
            INSERT INTO events (id, public_id, guild_id, channel_id, title, starts_at, status, created_by, created_at, updated_at)
            VALUES (11, marian_uuid_v7(), 1, 1, 'Raid', now(), 'draft', 1, now(), now()),
                   (12, marian_uuid_v7(), 1, 2, 'Officers', now(), 'draft', 1, now(), now());
            INSERT INTO activity (guild_id, event_id, body, created_at) VALUES
              (1, 11, 'Nell created Raid', now()),
              (1, 12, 'Nell created Officers', now());
            ",
        )
        .await.unwrap();
        let june = Viewer {
            membership_id: 2,
            discord_user_id: "u2".into(),
            ..nell.clone()
        };
        let officers = store::event(&mut conn, 12)
            .await
            .unwrap()
            .unwrap()
            .public_id
            .to_string();
        let all = dto::EventQuery {
            view: dto::EventView::All,
            ..Default::default()
        };
        let titles = async |conn: &mut Connection, viewer: &Viewer| -> Vec<String> {
            events(conn, viewer, &all)
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|e| e.title)
                .collect()
        };
        let channel_names = async |conn: &mut Connection, viewer: &Viewer| -> Vec<String> {
            channels(conn, viewer)
                .await
                .unwrap()
                .into_iter()
                .map(|c| c.name)
                .collect()
        };

        assert_eq!(titles(&mut conn, &june).await, ["Raid"]);
        assert_eq!(channel_names(&mut conn, &june).await, ["events"]);
        assert!(matches!(
            event(&mut conn, &june, &officers).await,
            Err(OpError::NotFound)
        ));
        assert!(matches!(
            event_activity(&mut conn, &june, &officers, 1, 25).await,
            Err(OpError::NotFound)
        ));

        // The server manager sees everything.
        assert_eq!(titles(&mut conn, &nell).await.len(), 2);
        assert_eq!(
            channel_names(&mut conn, &nell).await,
            ["events", "elsewhere"]
        );

        // Given Raider, June sees #elsewhere too.
        conn.batch_execute("INSERT INTO membership_roles (membership_id, role_id) VALUES (2, 1);")
            .await
            .unwrap();
        assert_eq!(titles(&mut conn, &june).await.len(), 2);
        assert!(event(&mut conn, &june, &officers).await.is_ok());
        assert_eq!(
            event_activity(&mut conn, &june, &officers, 1, 25)
                .await
                .unwrap()
                .total,
            1
        );
    }

    #[tokio::test]
    async fn the_event_list_filters_by_status_end_and_day() {
        let (mut conn, viewer) = world().await;
        let now = Utc::now();
        let mut add = async |title: &str, status: EventStatus, starts_at: DateTime<Utc>| {
            let id = logic::save_event(
                &mut conn,
                EventInput {
                    id: None,
                    guild_id: 1,
                    actor_id: 1,
                    channel_id: 1,
                    title: title.into(),
                    description: String::new(),
                    location: String::new(),
                    starts_at: now + Duration::days(30),
                    duration_minutes: 60,
                    color: "#1e3a6e".into(),
                    image_url: None,
                    cutoff_at: None,
                    attendee_role_id: None,
                    thread_enabled: false,
                    multiple_signups: false,
                    mention_everyone: false,
                    mention_role_ids: vec![],
                    restrict_role_ids: vec![],
                    options: vec![OptionInput::of_type(YES)],
                    reminders: vec![],
                    repeat: None,
                    action: logic::SaveAction::Draft,
                    post_at: None,
                    apply_series: false,
                },
            )
            .await
            .unwrap();
            diesel::update(crate::schema::events::table.find(id))
                .set((
                    crate::schema::events::status.eq(status),
                    crate::schema::events::starts_at.eq(starts_at),
                ))
                .execute(&mut conn)
                .await
                .unwrap();
        };
        add("old raid", EventStatus::Archived, now - Duration::days(20)).await;
        add("last raid", EventStatus::Posted, now - Duration::days(2)).await;
        add("next raid", EventStatus::Posted, now + Duration::days(2)).await;
        add("plan", EventStatus::Draft, now + Duration::days(5)).await;
        add("queued", EventStatus::Scheduled, now + Duration::days(9)).await;
        let titles = async |conn: &mut Connection, query: dto::EventQuery| -> Vec<String> {
            events(
                conn,
                &viewer,
                &dto::EventQuery {
                    view: dto::EventView::All,
                    ..query
                },
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|event| event.title)
            .collect()
        };
        let upcoming = dto::EventQuery {
            status: "scheduled,posted".into(),
            when: Some(dto::EventWhen::Upcoming),
            ..Default::default()
        };
        assert_eq!(titles(&mut conn, upcoming).await, ["next raid", "queued"]);
        let drafts = dto::EventQuery {
            status: "draft".into(),
            ..Default::default()
        };
        assert_eq!(titles(&mut conn, drafts).await, ["plan"]);
        let past = dto::EventQuery {
            when: Some(dto::EventWhen::Past),
            ..Default::default()
        };
        assert_eq!(
            titles(&mut conn, past).await,
            ["last raid", "old raid"],
            "newest first"
        );
        let warsaw: Tz = "Europe/Warsaw".parse().unwrap();
        let day = |at: DateTime<Utc>| at.with_timezone(&warsaw).date_naive();
        let days = dto::EventQuery {
            from_day: Some(day(now + Duration::days(2))),
            to_day: Some(day(now + Duration::days(5))),
            ..Default::default()
        };
        assert_eq!(
            titles(&mut conn, days).await,
            ["next raid", "plan"],
            "both days count"
        );
    }

    #[tokio::test]
    async fn a_patch_changes_only_what_it_names_and_keeps_the_event_posted() {
        let (mut conn, viewer) = world().await;
        // A fixed date clear of any clock change: moving the start by local hours
        // across one would land in a repeated or missing hour.
        let starts_at = chrono::TimeZone::with_ymd_and_hms(&Utc, 2030, 6, 12, 16, 0, 0).unwrap();
        let id = logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: "Bring food".into(),
                location: "Keep".into(),
                starts_at,
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: Some(starts_at - Duration::hours(1)),
                attendee_role_id: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                options: vec![OptionInput::of_type(YES)],
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .await
        .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).await.unwrap()[0].id;
        logic::signup(&mut conn, id, option, 2).await.unwrap();
        let public = event.public_id.to_string();

        let moved = event.starts_at + Duration::hours(2);
        let local = moved
            .with_timezone(&chrono_tz::Europe::Warsaw)
            .naive_local();
        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                title: Some("Raid night".into()),
                starts_local: Some(local),
                multiple_signups: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(after.title, "Raid night");
        assert_eq!(after.description, "Bring food");
        assert_eq!(after.location, "Keep");
        assert_eq!(after.status, EventStatus::Posted);
        assert!(after.multiple_signups);
        assert_eq!(after.starts_at.timestamp(), moved.timestamp());
        let cutoff = after.cutoff_at.unwrap();
        assert_eq!(
            (after.starts_at - cutoff).num_minutes(),
            60,
            "the cutoff moves with the start"
        );
        assert_eq!(
            store::options_for(&mut conn, &after).await.unwrap()[0].id,
            option
        );
        assert_eq!(
            store::signups_for(&mut conn, &after).await.unwrap().len(),
            1
        );

        diesel::update(crate::schema::events::table.filter(crate::schema::events::id.eq(id)))
            .set(crate::schema::events::signups_closed.eq(true))
            .execute(&mut conn)
            .await
            .unwrap();
        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                cutoff: Some(dto::Cutoff::Never),
                color: Some("#2f6b4f".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(after.cutoff_at, None);
        assert!(!after.signups_closed, "no cutoff opens signups again");
        assert_eq!(after.color, "#2f6b4f");
        assert_eq!(after.title, "Raid night");

        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                // A day before now, wherever the start is.
                cutoff: Some(dto::Cutoff::Before {
                    minutes: (moved - Utc::now()).num_minutes() + 24 * 60,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        assert!(
            after.signups_closed,
            "a cutoff already past keeps them closed"
        );

        // The API names roles and channels by public id, never the row key.
        let role = ids::public::roles(&mut conn, &[1]).await.unwrap().remove(0);
        assert_eq!(uuid::Uuid::parse_str(&role).unwrap().get_version_num(), 7);
        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                reminders: Some(vec![dto::Reminder {
                    offset_minutes: 120,
                    message: "Bring flasks".into(),
                    sent: false,
                }]),
                mention_everyone: Some(true),
                mention_role_ids: Some(vec![role.clone()]),
                restrict_role_ids: Some(vec![role.clone()]),
                attendee_role_id: Some(Some(role.clone())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        let reminders = store::reminders_for(&mut conn, &after).await.unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(
            (reminders[0].offset_minutes, reminders[0].message.as_str()),
            (120, "Bring flasks")
        );
        assert!(after.mention_everyone);
        assert_eq!(
            store::event_role_ids(&mut conn, &after, EventRoleKind::Mention)
                .await
                .unwrap(),
            [1]
        );
        assert_eq!(
            store::event_role_ids(&mut conn, &after, EventRoleKind::Restrict)
                .await
                .unwrap(),
            [1]
        );
        assert_eq!(after.attendee_role_id, Some(1));

        let cleared: dto::EventPatch =
            serde_json::from_str(r#"{"attendee_role_id":null}"#).unwrap();
        assert_eq!(cleared.attendee_role_id, Some(None));
        patch_event(&mut conn, &viewer, &public, &cleared)
            .await
            .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(after.attendee_role_id, None);
        assert_eq!(
            store::event_role_ids(&mut conn, &after, EventRoleKind::Mention)
                .await
                .unwrap(),
            [1],
            "unset fields stay"
        );

        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                reminders: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = store::event(&mut conn, id).await.unwrap().unwrap();
        assert!(
            store::reminders_for(&mut conn, &after)
                .await
                .unwrap()
                .is_empty(),
            "reminders can be cleared"
        );

        for (place, expected) in [("  Old keep  ", "Old keep"), ("", "")] {
            patch_event(
                &mut conn,
                &viewer,
                &public,
                &dto::EventPatch {
                    location: Some(place.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                store::event(&mut conn, id).await.unwrap().unwrap().location,
                expected
            );
        }

        let updates: i64 = crate::schema::activity::table
            .filter(crate::schema::activity::body.eq("Updated Raid night."))
            .count()
            .get_result(&mut conn)
            .await
            .unwrap();
        assert_eq!(updates, 1, "quick edits fold into one line");

        let err = patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                title: Some(" ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("title"));
    }

    #[tokio::test]
    async fn a_draft_can_be_scheduled_or_posted_from_its_page() {
        let (mut conn, viewer) = world().await;
        let starts_at = Utc::now() + Duration::days(3);
        let id = logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: String::new(),
                location: String::new(),
                starts_at,
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: None,
                attendee_role_id: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                options: vec![OptionInput::of_type(YES)],
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Draft,
                post_at: None,
                apply_series: false,
            },
        )
        .await
        .unwrap();
        let public = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .public_id
            .to_string();
        let local = |at: DateTime<Utc>| at.with_timezone(&chrono_tz::Europe::Warsaw).naive_local();

        let past = dto::PostAt {
            post_local: local(Utc::now() - Duration::hours(1)),
        };
        let err = schedule_event(&mut conn, &viewer, &public, &past)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("past"), "{err}");

        let post_at = starts_at - Duration::days(1);
        schedule_event(
            &mut conn,
            &viewer,
            &public,
            &dto::PostAt {
                post_local: local(post_at),
            },
        )
        .await
        .unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Scheduled);
        assert_eq!(
            event.post_at.unwrap().timestamp() / 60,
            post_at.timestamp() / 60
        );

        unschedule_event(&mut conn, &viewer, &public).await.unwrap();
        let event = store::event(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(
            (event.status, event.post_at),
            (EventStatus::Draft, None),
            "back to a draft"
        );

        // Before posting, an option can go.
        let mut options = store::options_for(&mut conn, &event).await.unwrap();
        assert_eq!(options.len(), 1);
        let extra = OptionInput::of_type(MAYBE);
        let keep = OptionInput {
            id: Some(options[0].id),
            ..OptionInput::of_type(options[0].type_id)
        };
        logic::save_options(&mut conn, id, 1, &[keep.clone(), extra])
            .await
            .unwrap();
        options = store::options_for(&mut conn, &event).await.unwrap();
        assert_eq!(options.len(), 2);
        logic::save_options(&mut conn, id, 1, &[keep])
            .await
            .unwrap();
        assert_eq!(
            store::options_for(&mut conn, &event).await.unwrap().len(),
            1
        );

        publish_event(&mut conn, &viewer, &public).await.unwrap();
        assert_eq!(
            store::event(&mut conn, id).await.unwrap().unwrap().status,
            EventStatus::Posted
        );
        let moved = dto::EventPatch {
            channel_id: Some(ids::public::channel(&mut conn, 2).await.unwrap()),
            ..Default::default()
        };
        let err = patch_event(&mut conn, &viewer, &public, &moved)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("stays in its channel"), "{err}");
        let err = publish_event(&mut conn, &viewer, &public)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("already gone out"), "{err}");
        let err = schedule_event(
            &mut conn,
            &viewer,
            &public,
            &dto::PostAt {
                post_local: local(post_at),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("already gone out"), "{err}");
    }

    #[tokio::test]
    async fn an_event_starts_repeating_from_its_page() {
        let (mut conn, viewer) = world().await;
        let id = logic::save_event(
            &mut conn,
            EventInput {
                id: None,
                guild_id: 1,
                actor_id: 1,
                channel_id: 1,
                title: "Raid".into(),
                description: String::new(),
                location: String::new(),
                starts_at: Utc::now() + Duration::days(2),
                duration_minutes: 60,
                color: "#1e3a6e".into(),
                image_url: None,
                cutoff_at: None,
                attendee_role_id: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                options: vec![OptionInput::of_type(YES)],
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .await
        .unwrap();
        let public = store::event(&mut conn, id)
            .await
            .unwrap()
            .unwrap()
            .public_id
            .to_string();
        assert!(
            event(&mut conn, &viewer, &public)
                .await
                .unwrap()
                .series
                .is_none()
        );

        let weekly = dto::EventPatch {
            repeat: Some(dto::Repeat {
                cadence: "weekly".into(),
                count: Some(3),
                until: None,
            }),
            ..Default::default()
        };
        patch_event(&mut conn, &viewer, &public, &weekly)
            .await
            .unwrap();
        let shown = event(&mut conn, &viewer, &public).await.unwrap();
        let series = shown.series.expect("the event now belongs to a series");
        assert_eq!(series.cadence, Cadence::Weekly);
        assert_eq!(series.max_occurrences, Some(3));
        assert!(series.active);
        assert_eq!(shown.status, EventStatus::Posted);

        let err = patch_event(&mut conn, &viewer, &public, &weekly)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("already belongs to a series"), "{err}");

        // Someone who can't manage the server doesn't get the list, and gets
        // one series only once they can edit a date in it.
        let june = Viewer {
            membership_id: 2,
            ..viewer.clone()
        };
        assert_eq!(series_list(&mut conn, &viewer).await.unwrap().len(), 1);
        assert!(series_list(&mut conn, &june).await.is_err());
        assert!(matches!(
            self::series(&mut conn, &june, &series.id).await,
            Err(OpError::NotFound)
        ));
        conn.batch_execute(&format!("UPDATE events SET created_by = 2 WHERE id = {id}"))
            .await
            .unwrap();
        assert!(self::series(&mut conn, &june, &series.id).await.is_ok());
    }

    #[tokio::test]
    async fn only_a_server_manager_reads_the_server_wide_pages() {
        let (mut conn, nell) = world().await;
        let june = Viewer {
            membership_id: 2,
            discord_user_id: "u2".into(),
            ..nell.clone()
        };
        assert!(activity(&mut conn, &nell, 1, 25).await.is_ok());
        assert!(routines(&mut conn, &nell).await.is_ok());
        assert!(activity(&mut conn, &june, 1, 25).await.is_err());
        assert!(routines(&mut conn, &june).await.is_err());
    }

    /// A routine made on the web whose closed poll asks the Raiders in #elsewhere.
    async fn approved_routine(conn: &mut Connection, viewer: &Viewer) -> String {
        let channels = channels(conn, viewer).await.unwrap();
        let channel = |name: &str| {
            channels
                .iter()
                .find(|channel| channel.name == name)
                .unwrap()
                .id
                .clone()
        };
        let raider = roles(conn, viewer).await.unwrap()[0].id.clone();
        let step = |kind: &str| dto::RoutineStep {
            kind: kind.into(),
            ..Default::default()
        };
        let draft = dto::RoutineDraft {
            channel_id: channel("events"),
            title: "Raid night".into(),
            duration_minutes: 120,
            start_time: "19:00".into(),
            poll_weekday: 0,
            poll_time: "00:00".into(),
            lock_weekday: 6,
            lock_time: "23:59".into(),
            days: (0..7).collect(),
            steps: Some(vec![
                step("poll"),
                dto::RoutineStep {
                    kind: "approval".into(),
                    channel_id: Some(channel("elsewhere")),
                    role_ids: vec![raider],
                },
                step("post"),
            ]),
            ..Default::default()
        };
        let made = create_routine(conn, viewer, &draft).await.unwrap();
        made.done
            .location
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn a_routine_made_on_the_web_waits_for_its_approval_there() {
        let (mut conn, viewer) = world().await;
        let id = approved_routine(&mut conn, &viewer).await;
        let shown = routine(&mut conn, &viewer, &id).await.unwrap();
        let kinds: Vec<&str> = shown.steps.iter().map(|step| step.kind.as_str()).collect();
        assert_eq!(kinds, ["poll", "approval", "post"]);
        assert!(shown.approval.is_none());

        // The poll closes now.
        conn.batch_execute("UPDATE routine_runs SET locks_at = now() - interval '1 minute'")
            .await
            .unwrap();
        logic::maintain_routines(&mut conn, 1, Utc::now())
            .await
            .unwrap();
        let shown = routine(&mut conn, &viewer, &id).await.unwrap();
        let approval = shown.approval.expect("the poll waits for approval");
        assert_eq!(approval.channel, "elsewhere");
        assert!(approval.can_decide, "a server manager may answer");

        let mut june = viewer.clone();
        june.membership_id = 2;
        assert!(
            matches!(routine(&mut conn, &june, &id).await, Err(OpError::NotFound)),
            "not hers to answer"
        );
        assert!(
            decide_routine(&mut conn, &june, &id, &dto::Decision { day: None })
                .await
                .is_err()
        );
        // A Raider may answer, so Discord's link opens the routine for her.
        conn.batch_execute("INSERT INTO membership_roles (membership_id, role_id) VALUES (2, 1);")
            .await
            .unwrap();
        assert!(
            routine(&mut conn, &june, &id)
                .await
                .unwrap()
                .approval
                .unwrap()
                .can_decide
        );
        conn.batch_execute("DELETE FROM membership_roles WHERE membership_id = 2;")
            .await
            .unwrap();

        decide_routine(&mut conn, &viewer, &id, &dto::Decision { day: None })
            .await
            .unwrap();
        assert!(
            routine(&mut conn, &viewer, &id)
                .await
                .unwrap()
                .approval
                .is_none()
        );
        let again = decide_routine(&mut conn, &viewer, &id, &dto::Decision { day: None })
            .await
            .unwrap_err();
        assert_eq!(again.to_string(), "This request has already been answered.");

        // Saving the routine without its steps keeps them; sending two steps drops the approval.
        let mut draft = dto::RoutineDraft {
            channel_id: shown.channel_id.clone(),
            title: shown.title.clone(),
            duration_minutes: shown.duration_minutes,
            start_time: shown.start_time.clone(),
            poll_weekday: shown.poll_weekday.parse().unwrap(),
            poll_time: shown.poll_time.clone(),
            lock_weekday: shown.lock_weekday.parse().unwrap(),
            lock_time: shown.lock_time.clone(),
            days: (0..7).collect(),
            ..Default::default()
        };
        update_routine(&mut conn, &viewer, &id, &draft)
            .await
            .unwrap();
        assert_eq!(
            routine(&mut conn, &viewer, &id).await.unwrap().steps.len(),
            3
        );
        draft.steps = Some(vec![
            dto::RoutineStep {
                kind: "poll".into(),
                ..Default::default()
            },
            dto::RoutineStep {
                kind: "post".into(),
                ..Default::default()
            },
        ]);
        update_routine(&mut conn, &viewer, &id, &draft)
            .await
            .unwrap();
        assert_eq!(
            routine(&mut conn, &viewer, &id).await.unwrap().steps.len(),
            2
        );
    }

    #[tokio::test]
    async fn the_settings_page_picks_the_discord_language() {
        let (mut conn, viewer) = world().await;
        let mut next = settings(&mut conn, &viewer).await.unwrap();
        assert_eq!(next.language, "en");

        next.language = "pl".into();
        let changed = put_settings(&mut conn, &viewer, &next).await.unwrap();
        assert_eq!(settings(&mut conn, &viewer).await.unwrap().language, "pl");
        assert!(
            changed.jobs.is_empty(),
            "the commands already list Polish, so nothing to register"
        );

        next.language = String::new();
        put_settings(&mut conn, &viewer, &next).await.unwrap();
        assert_eq!(
            settings(&mut conn, &viewer).await.unwrap().language,
            "pl",
            "an empty language keeps it"
        );

        next.language = "de".into();
        assert!(put_settings(&mut conn, &viewer, &next).await.is_err());
        assert_eq!(settings(&mut conn, &viewer).await.unwrap().language, "pl");
    }

    #[tokio::test]
    async fn settings_that_cannot_all_be_saved_change_nothing() {
        let (mut conn, viewer) = world().await;
        let mut next = settings(&mut conn, &viewer).await.unwrap();
        next.timezone = "America/New_York".into();
        next.waitlist = !next.waitlist;
        let raider = store::role(&mut conn, 1).await.unwrap().unwrap();
        next.role_event = vec![raider.public_id.to_string()];
        refuse(&mut conn, "INSERT", "permission_roles").await;
        assert!(put_settings(&mut conn, &viewer, &next).await.is_err());
        let after = settings(&mut conn, &viewer).await.unwrap();
        assert_eq!(after.timezone, "Europe/Warsaw");
        assert_ne!(after.waitlist, next.waitlist);
    }
}
