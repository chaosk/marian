//! What the web pages and the REST API read and change, as plain resources.
//!
//! Each function works for one signed-in person bound to one server.

use std::collections::HashMap;

use axum::http::{header, HeaderMap};
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

pub fn viewer(conn: &mut Connection, headers: &HeaderMap) -> Option<Viewer> {
    let token = cookie(headers, "marian_session")?;
    let row = store::web_session(conn, &token).ok()??;
    let fresh = Utc::now() - row.created_at < Duration::days(30);
    if !fresh {
        let _ = store::delete_web_session(conn, &token);
        return None;
    }
    let user = store::membership(conn, row.membership_id).ok()??;
    let discord_id = store::discord_id_for_membership(conn, user.id).ok()?;
    if user.guild_id != row.guild_id || discord_id != row.discord_user_id {
        let _ = store::delete_web_session(conn, &token);
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
pub fn bind_server(conn: &mut Connection, viewer: &mut Viewer, server: &str) -> Result<(), OpError> {
    let guild = store::guild_by_discord(conn, server)?.ok_or(OpError::NotFound)?;
    bind_guild(conn, viewer, guild.id)
}

/// A server the viewer didn't sign into reads as one that isn't there, so
/// nobody can learn which servers Marian is in by asking for them.
pub fn bind_guild(conn: &mut Connection, viewer: &mut Viewer, guild_id: i64) -> Result<(), OpError> {
    if guild_id <= 0 || !viewer.allowed.contains(&guild_id) || store::guild(conn, guild_id)?.is_none() {
        return Err(OpError::NotFound);
    }
    let user = logic::ensure_discord_user(
        conn,
        guild_id,
        &viewer.discord_user_id,
        &viewer.display_name,
    )?;
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

fn server_id(conn: &mut Connection, viewer: &Viewer) -> Result<String, OpError> {
    store::discord_server_id(conn, viewer.guild_id)
}

fn path(conn: &mut Connection, viewer: &Viewer, rest: &str) -> Result<String, OpError> {
    Ok(format!("/servers/{}{rest}", server_id(conn, viewer)?))
}

fn zone(conn: &mut Connection, guild_id: i64) -> Result<Tz, OpError> {
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    clock::zone(&guild.timezone)
}

fn must_member(conn: &mut Connection, viewer: &Viewer) -> Result<store::Membership, OpError> {
    store::membership(conn, viewer.membership_id)?.ok_or(OpError::NotFound)
}

fn event_here(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<store::Event, OpError> {
    let event = store::event_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if event.guild_id != viewer.guild_id {
        return Err(OpError::NotFound);
    }
    Ok(event)
}

fn series_here(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<store::Series, OpError> {
    let series = store::series_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if series.guild_id != viewer.guild_id {
        return Err(OpError::NotFound);
    }
    Ok(series)
}

pub fn me(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Me, OpError> {
    let servers = store::list_guilds(conn)?
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

pub fn server(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Server, OpError> {
    let guild = store::guild(conn, viewer.guild_id)?.ok_or(OpError::NotFound)?;
    let settings = store::settings(conn, viewer.guild_id)?;
    let counts = dto::Counts {
        drafts: store::count_status(conn, viewer.guild_id, "draft")?,
        upcoming: store::count_upcoming(conn, viewer.guild_id)?,
        scheduled: store::count_status(conn, viewer.guild_id, "scheduled")?
            + store::count_status(conn, viewer.guild_id, "failed")?,
        series: store::count_active_series(conn, viewer.guild_id)?,
        routines: logic::count_active(conn, viewer.guild_id)?,
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

pub fn membership(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Membership, OpError> {
    let user = must_member(conn, viewer)?;
    let token = store::ensure_calendar_token(conn, user.user_id)?;
    Ok(dto::Membership {
        name: user.name,
        server_manager: user.server_manager,
        reminders_enabled: user.reminders_enabled,
        calendar_url: format!("/calendar/{token}.ics"),
        google: google(conn, user.user_id)?,
    })
}

fn google(conn: &mut Connection, user_id: i64) -> Result<dto::Google, OpError> {
    let status = crate::google::status(conn, user_id)?;
    Ok(dto::Google {
        ready: crate::google::configured_from_env(),
        connected: status.as_ref().is_some_and(|s| s.connected),
        reconnect: status.as_ref().is_some_and(|s| s.reconnect),
        auto_sync: status.as_ref().is_some_and(|s| s.auto_sync),
        email: status.map(|s| s.email).unwrap_or_default(),
    })
}

pub fn patch_membership(
    conn: &mut Connection,
    viewer: &Viewer,
    patch: &dto::MembershipPatch,
) -> Result<Outcome, OpError> {
    if let Some(enabled) = patch.reminders_enabled {
        logic::set_user_reminders(conn, viewer.membership_id, enabled)?;
    }
    if let Some(enabled) = patch.google_auto_sync {
        crate::commands::google_auto_sync(
            conn,
            viewer.guild_id,
            viewer.membership_id,
            enabled,
        )?;
    }
    Ok(Outcome::ok())
}

pub fn settings(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Settings, OpError> {
    let guild = store::guild(conn, viewer.guild_id)?.ok_or(OpError::NotFound)?;
    let s = store::settings(conn, viewer.guild_id)?;
    let public = |conn: &mut Connection, ids: &[i64]| ids::public::roles(conn, ids);
    Ok(dto::Settings {
        role_event: public(conn, &s.role_event)?,
        role_edit: public(conn, &s.role_edit)?,
        role_delete: public(conn, &s.role_delete)?,
        role_channel: public(conn, &s.role_channel)?,
        role_signup: public(conn, &s.role_signup)?,
        timezone: guild.timezone,
        default_reminder_minutes: s.default_reminder_minutes,
        auto_archive_hours: s.auto_archive_hours,
        post_lead_hours: s.post_lead_hours,
        digest: s.digest,
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
    })
}

pub fn put_settings(
    conn: &mut Connection,
    viewer: &Viewer,
    next: &dto::Settings,
) -> Result<Outcome, OpError> {
    let user = must_member(conn, viewer)?;
    if !user.server_manager {
        return Err(OpError::message(
            "Only someone who runs the server can change settings.",
        ));
    }
    clock::zone(&next.timezone)
        .map_err(|_| OpError::message("That timezone isn't one Marian knows."))?;
    let mut s = store::settings(conn, viewer.guild_id)?;
    // Zero is "no default reminder".
    s.default_reminder_minutes = bounded("The default reminder", next.default_reminder_minutes, 0, 10080)?;
    s.auto_archive_hours = bounded("Archive after", next.auto_archive_hours, 1, 168)?;
    s.post_lead_hours = bounded("Post lead", next.post_lead_hours, 0, 336)?;
    s.digest = match next.digest.as_str() {
        "daily" | "weekly" => next.digest.clone(),
        _ => "off".into(),
    };
    // Roles by public id. One the server no longer has is dropped.
    let roles: std::collections::HashMap<String, i64> = store::list_roles(conn, viewer.guild_id)?
        .into_iter()
        .map(|role| (role.public_id.to_string(), role.id))
        .collect();
    let known = |ids: &[String]| -> Vec<i64> { ids.iter().filter_map(|id| roles.get(id).copied()).collect() };
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
    store::set_guild_timezone(conn, viewer.guild_id, &next.timezone)?;
    store::update_settings(conn, &s)?;
    Ok(Outcome::ok())
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

pub fn roles(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Role>, OpError> {
    Ok(store::list_roles(conn, viewer.guild_id)?
        .into_iter()
        .map(|role| dto::Role {
            id: role.public_id.to_string(),
            name: role.name,
        })
        .collect())
}

pub fn members(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Member>, OpError> {
    Ok(store::list_memberships(conn, viewer.guild_id)?
        .into_iter()
        .map(|person| dto::Member {
            id: person.public_id.to_string(),
            name: person.name,
            color: person.color,
        })
        .collect())
}

/// People found in the Discord server, as members here. Anyone new gets the
/// membership they would get the first time they pressed a button.
pub fn remember_members(
    conn: &mut Connection,
    viewer: &Viewer,
    found: &[(String, String)],
) -> Result<Vec<dto::Member>, OpError> {
    found
        .iter()
        .map(|(discord_id, name)| {
            let person = logic::ensure_discord_user(conn, viewer.guild_id, discord_id, name)?;
            Ok(dto::Member {
                id: person.public_id.to_string(),
                name: person.name,
                color: person.color,
            })
        })
        .collect()
}

pub fn channels(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Channel>, OpError> {
    let events = store::list_events(conn, viewer.guild_id)?;
    Ok(store::list_channels(conn, viewer.guild_id)?
        .into_iter()
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
        dto::EventView::Upcoming => match event.status.as_str() {
            "posted" => !ended,
            "scheduled" => true,
            _ => false,
        },
        dto::EventView::Wrapped => event.status == "posted" && ended,
        dto::EventView::Drafts => event.status == "draft",
        dto::EventView::Queued => matches!(event.status.as_str(), "scheduled" | "failed"),
        dto::EventView::Archive => {
            matches!(event.status.as_str(), "archived" | "cancelled" | "skipped")
        }
        dto::EventView::All => true,
    }
}

pub const PER_PAGE: u32 = 25;

pub fn events(
    conn: &mut Connection,
    viewer: &Viewer,
    query: &dto::EventQuery,
) -> Result<dto::Paged<dto::EventSummary>, OpError> {
    let now = Utc::now();
    let q = query.q.trim().to_lowercase();
    let series_id = match query.series.as_deref() {
        Some(id) => Some(series_here(conn, viewer, id)?.id),
        None => None,
    };
    let statuses: Vec<&str> = query
        .status
        .split(',')
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .collect();
    let tz = zone(conn, viewer.guild_id)?;
    let midnight = |day: chrono::NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .and_then(|naive| naive.and_local_timezone(tz).earliest())
            .map(|at| at.with_timezone(&Utc))
    };
    let from_day = query.from_day.and_then(midnight);
    let to_day = query.to_day.and_then(|day| day.succ_opt()).and_then(midnight);
    let mut events: Vec<store::Event> = match series_id {
        Some(id) => store::events_in_series(conn, id)?,
        None => store::list_events(conn, viewer.guild_id)?,
    }
    .into_iter()
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
    let options = store::options_by_event(conn, &shown)?;
    let signups = store::signups_by_event(conn, &shown)?;
    let channels: HashMap<i64, String> = store::list_channels(conn, viewer.guild_id)?
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
            status: event.status.clone(),
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
        .map(|opt| {
            let count = |state: &str| {
                signups
                    .iter()
                    .filter(|s| s.option_id == opt.id && s.state == state)
                    .count() as i64
            };
            dto::Spot {
                emoji: opt.emoji.clone(),
                name: opt.name.clone(),
                capacity: opt.capacity,
                confirmed: count("confirmed"),
                waiting: count("waitlisted"),
            }
        })
        .collect()
}

pub fn event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Event, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let settings = store::settings(conn, viewer.guild_id)?;
    let user = must_member(conn, viewer)?;
    let roles = store::membership_role_ids(conn, &user)?;
    let can_edit = logic::can_edit(&user, &event, &settings, &roles);
    let channel = store::channel(conn, event.channel_id)?;
    let channel_name = channel
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "deleted".into());
    let server = server_id(conn, viewer)?;
    let discord_url = (|| {
        let channel_id = channel.as_ref()?.discord_id.as_deref()?.trim();
        let message_id = event.discord_message_id.as_deref()?.trim();
        if channel_id.is_empty() || message_id.is_empty() || server.is_empty() {
            return None;
        }
        Some(logic::discord_message_link(&server, channel_id, message_id))
    })();
    let series = match event.series_id {
        Some(id) => match store::series(conn, id)? {
            Some(series) => Some(series_dto(conn, series)?),
            None => None,
        },
        None => None,
    };
    let creator = match event.created_by {
        Some(id) => store::membership(conn, id)?
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
        status: event.status.clone(),
        starts_at: event.starts_at,
        duration_minutes: event.duration_minutes,
        cutoff_at: event.cutoff_at,
        post_at: event.post_at,
        color: event.color.clone(),
        image_url,
        channel_id: ids::public::channel(conn, event.channel_id)?,
        channel: channel_name.clone(),
        attendee_role_id: ids::public::role(conn, event.attendee_role_id)?,
        mention_everyone: event.mention_everyone,
        mention_role_ids: {
            let ids = store::event_role_ids(conn, &event, "mention")?;
            ids::public::roles(conn, &ids)?
        },
        restrict_role_ids: {
            let ids = store::event_role_ids(conn, &event, "restrict")?;
            ids::public::roles(conn, &ids)?
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
        google_synced: crate::google::on_calendar(conn, user.user_id, event.id)?,
        can: dto::Abilities {
            edit: can_edit,
            delete: logic::can_delete(&user, &event, &settings, &roles),
            archive: can_edit && event.status == "posted",
            skip: can_edit
                && event.series_id.is_some()
                && matches!(event.status.as_str(), "scheduled" | "draft"),
        },
    })
}

/// Someone who can edit the event adding, changing, or reordering its options.
pub fn put_options(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    options: &[dto::SignupOption],
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let input = option_inputs(conn, viewer.guild_id, Some(event.id), options)?;
    logic::save_options(conn, event.id, viewer.membership_id, &input)?;
    Ok(Outcome::ok().synced(event.id))
}

pub fn event_reminders(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Vec<dto::Reminder>, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    Ok(store::reminders_for(conn, &event)?
        .into_iter()
        .map(|rem| dto::Reminder {
            offset_minutes: rem.offset_minutes,
            message: rem.message,
            sent: rem.sent_at.is_some(),
        })
        .collect())
}

pub fn signups(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Signups, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let settings = store::settings(conn, viewer.guild_id)?;
    let user = must_member(conn, viewer)?;
    let roles = store::membership_role_ids(conn, &user)?;
    let can_manage = logic::can_edit(&user, &event, &settings, &roles);
    let options = store::options_for(conn, &event)?;
    let signups = store::signups_for(conn, &event)?;
    let people = store::list_memberships(conn, viewer.guild_id)?;
    let role_rows = store::list_roles(conn, viewer.guild_id)?;
    let restrict_ids = store::event_role_ids(conn, &event, "restrict")?;
    let now = Utc::now();
    let closed = event.status != "posted" || event.signups_closed || cutoff_passed(&event, now);
    let show_names = settings.show_attendees || can_manage;
    let named = |state: &str, option_id: i64| -> Vec<dto::Person> {
        if !show_names {
            return Vec::new();
        }
        signups
            .iter()
            .filter(|s| s.option_id == option_id && s.state == state)
            .filter_map(|s| people.iter().find(|p| p.id == s.membership_id))
            .map(|p| dto::Person {
                id: p.public_id.to_string(),
                name: p.name.clone(),
                color: p.color.clone(),
            })
            .collect()
    };
    let rows = options
        .iter()
        .map(|opt| {
            let confirmed = signups
                .iter()
                .filter(|s| s.option_id == opt.id && s.state == "confirmed")
                .count();
            let mine = signups
                .iter()
                .any(|s| s.option_id == opt.id && s.membership_id == user.id);
            let gate = Gate {
                event: &event,
                user: &user,
                roles: &roles,
                restrict_ids: &restrict_ids,
                closed,
                waitlist: settings.waitlist,
            };
            let (open, reason, waitlist_next) = gate.button(opt, mine, confirmed);
            let role = opt.role_id.and_then(|id| role_rows.iter().find(|r| r.id == id));
            dto::OptionSignups {
                id: opt.public_id.to_string(),
                type_id: opt.type_public_id.to_string(),
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                role_id: role.map(|r| r.public_id.to_string()),
                role: role.map(|r| r.name.clone()),
                attending: opt.reminders_enabled,
                confirmed_count: confirmed as i64,
                waiting_count: signups
                    .iter()
                    .filter(|s| s.option_id == opt.id && s.state == "waitlisted")
                    .count() as i64,
                confirmed: named("confirmed", opt.id),
                waiting: named("waitlisted", opt.id),
                mine,
                open,
                reason,
                waitlist_next,
            }
        })
        .collect();
    Ok(dto::Signups {
        options: rows,
        notes: signup_notes(&event, &options, &signups, &user, &roles, &restrict_ids, &role_rows, settings.waitlist, now),
        show_names,
        can_manage,
        editable: can_manage && !matches!(event.status.as_str(), "archived" | "skipped" | "cancelled"),
        posted: event.status == "posted",
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
    fn button(&self, opt: &store::SignupOption, mine: bool, confirmed: usize) -> (bool, String, bool) {
        if mine {
            return (self.event.status == "posted", String::new(), false);
        }
        if self.event.status != "posted" {
            return (false, "Signups open once the event is posted.".into(), false);
        }
        if self.closed {
            return (false, "Signups are closed.".into(), false);
        }
        if !self.restrict_ids.is_empty()
            && !self.user.server_manager
            && !self.restrict_ids.iter().any(|id| self.roles.contains(id))
        {
            return (false, "This event is limited to another role.".into(), false);
        }
        if let Some(role_id) = opt.role_id {
            if !self.user.server_manager && !self.roles.contains(&role_id) {
                return (false, "Your roles don't include this option.".into(), false);
            }
        }
        if let Some(cap) = opt.capacity {
            if confirmed as i64 >= cap {
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
    if matches!(event.status.as_str(), "archived" | "skipped" | "cancelled") {
        return Vec::new();
    }
    let mut notes = Vec::new();
    if event.status != "posted" {
        notes.push("Signups open once the event is posted.".to_string());
    }
    if event.signups_closed || cutoff_passed(event, now) {
        notes.push("Signups are closed.".into());
    }
    if !restrict_ids.is_empty() && !user.server_manager && !restrict_ids.iter().any(|id| roles.contains(id)) {
        notes.push("This event is limited to another role.".into());
    }
    for opt in options {
        let yours = signups.iter().any(|s| {
            s.option_id == opt.id && s.membership_id == user.id && s.state == "confirmed"
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
            let confirmed = signups
                .iter()
                .filter(|s| s.option_id == opt.id && s.state == "confirmed")
                .count() as i64;
            if confirmed >= cap && !waitlist {
                notes.push(format!("{} is full.", opt.name));
            }
        }
    }
    if notes.len() > 1 {
        notes.retain(|note| note != "Signups open once the event is posted.");
    }
    notes
}

fn cutoff_passed(event: &store::Event, now: DateTime<Utc>) -> bool {
    event.cutoff_at.is_some_and(|dt| now >= dt)
}

fn signed_up(conn: &mut Connection, event: &store::Event, option_id: i64, membership_id: i64) -> Result<bool, OpError> {
    Ok(store::signups_for(conn, event)?
        .iter()
        .any(|s| s.option_id == option_id && s.membership_id == membership_id))
}

/// Signs the viewer up for an option, or takes them off it.
pub fn set_my_signup(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    option: &str,
    on: bool,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let option_id = ids::public::option_id(conn, event.id, option)?;
    if signed_up(conn, &event, option_id, viewer.membership_id)? == on {
        return Ok(Outcome::ok());
    }
    logic::signup(conn, event.id, option_id, viewer.membership_id)?;
    Ok(Outcome::ok().synced(event.id))
}

/// Someone who can edit the event adding a person to an option, or taking them off it.
pub fn set_signup(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    member: &str,
    option: &str,
    on: bool,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let member_id = ids::public::member_id(conn, viewer.guild_id, member)?;
    let option_id = ids::public::option_id(conn, event.id, option)?;
    logic::editor_response(
        conn,
        event.id,
        viewer.membership_id,
        member_id,
        option_id,
        if on { "add" } else { "remove" },
    )?;
    Ok(Outcome::ok().synced(event.id))
}

pub fn pending_reminders(
    conn: &mut Connection,
    viewer: &Viewer,
    limit: usize,
) -> Result<Vec<dto::PendingReminder>, OpError> {
    let events: Vec<store::Event> = store::list_events(conn, viewer.guild_id)?
        .into_iter()
        .filter(|event| event.status == "posted")
        .collect();
    let groups = store::reminders_by_event(conn, &events)?;
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

pub fn activity(
    conn: &mut Connection,
    viewer: &Viewer,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    activity_of(conn, viewer, None, page, limit)
}

/// What happened to one event, newest first.
pub fn event_activity(
    conn: &mut Connection,
    viewer: &Viewer,
    id: &str,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    let event = event_here(conn, viewer, id)?;
    activity_of(conn, viewer, Some(event.id), page, limit)
}

fn activity_of(
    conn: &mut Connection,
    viewer: &Viewer,
    event_id: Option<i64>,
    page: u32,
    limit: i64,
) -> Result<dto::Paged<dto::Activity>, OpError> {
    let per_page = limit.clamp(1, 200) as u32;
    let total = store::count_activity(conn, viewer.guild_id, event_id)? as usize;
    let pages = (total as u32).div_ceil(per_page).max(1);
    let page = page.clamp(1, pages);
    let offset = i64::from((page - 1) * per_page);
    let mut items = Vec::new();
    for item in store::list_activity(conn, viewer.guild_id, event_id, i64::from(per_page), offset)? {
        let event_id = match item.event_id {
            Some(id) => store::event(conn, id)?.map(|event| event.public_id.to_string()),
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

fn event_input(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::EventDraft,
    existing: Option<&store::Event>,
) -> Result<EventInput, OpError> {
    let tz = zone(conn, viewer.guild_id)?;
    let in_series = existing.is_some_and(|event| event.series_id.is_some());
    let repeat = match &draft.repeat {
        Some(repeat) if !in_series && !repeat.cadence.is_empty() && repeat.cadence != "none" => {
            Some(RepeatSpec {
                cadence: repeat.cadence.clone(),
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
    let options = option_inputs(conn, viewer.guild_id, existing.map(|event| event.id), &named)?;
    Ok(EventInput {
        id: existing.map(|event| event.id),
        guild_id: viewer.guild_id,
        actor_id: viewer.membership_id,
        channel_id: ids::public::channel_id(conn, viewer.guild_id, &draft.channel_id)?,
        title: draft.title.clone(),
        description: draft.description.clone(),
        location: draft.location.clone(),
        starts_at: local_instant(draft.starts_local, tz)?,
        duration_minutes: draft.duration_minutes,
        color: draft.color.clone(),
        image_url: None,
        cutoff_at: draft.cutoff_local.map(|dt| local_instant(dt, tz)).transpose()?,
        attendee_role_id: ids::public::role_id(conn, viewer.guild_id, draft.attendee_role_id.as_deref())?,
        thread_enabled: draft.thread_enabled,
        multiple_signups: draft.multiple_signups,
        mention_everyone: draft.mention_everyone,
        mention_role_ids: ids::public::role_ids(conn, viewer.guild_id, &draft.mention_role_ids)?,
        restrict_role_ids: ids::public::role_ids(conn, viewer.guild_id, &draft.restrict_role_ids)?,
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
        post_at: draft.post_local.map(|dt| local_instant(dt, tz)).transpose()?,
        apply_series: draft.apply_series,
    })
}

fn saved(conn: &mut Connection, viewer: &Viewer, event_id: i64) -> Result<Outcome, OpError> {
    let event = store::event(conn, event_id)?.ok_or(OpError::NotFound)?;
    let location = path(conn, viewer, &format!("/events/{}", event.public_id))?;
    Ok(Outcome::ok().at(location).synced(event_id))
}

pub fn create_event(conn: &mut Connection, viewer: &Viewer, draft: &dto::EventDraft) -> Result<Outcome, OpError> {
    let input = event_input(conn, viewer, draft, None)?;
    let id = logic::save_event(conn, input)?;
    saved(conn, viewer, id)
}

pub fn update_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::EventDraft,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let input = event_input(conn, viewer, draft, Some(&event))?;
    let id = logic::save_event(conn, input)?;
    saved(conn, viewer, id)
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

pub fn patch_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    patch: &dto::EventPatch,
) -> Result<Outcome, OpError> {
    save_stored(conn, viewer, public_id, patch, None)
}

/// Posts a draft or a scheduled event to Discord right away.
pub fn publish_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    save_stored(conn, viewer, public_id, &dto::EventPatch::default(), Some(Posting::Now))
}

/// Takes a scheduled or failed post back to a draft.
pub fn unschedule_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    save_stored(conn, viewer, public_id, &dto::EventPatch::default(), Some(Posting::Hold))
}

/// Sets when Marian posts a draft, or moves a scheduled post.
pub fn schedule_event(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    when: &dto::PostAt,
) -> Result<Outcome, OpError> {
    save_stored(conn, viewer, public_id, &dto::EventPatch::default(), Some(Posting::At(when.post_local)))
}

/// Saves an event from what is stored, with `patch` on top and, when it has
/// not gone out yet, a change of when it posts. Everything else stays as it is.
fn save_stored(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    patch: &dto::EventPatch,
    posting: Option<Posting>,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    if posting.is_some() && !matches!(event.status.as_str(), "draft" | "scheduled" | "failed") {
        return Err(OpError::message("This event has already gone out."));
    }
    let unposted = matches!(event.status.as_str(), "draft" | "scheduled" | "failed") && event.discord_message_id.is_none();
    let channel_id = match &patch.channel_id {
        Some(public) => ids::public::channel_id(conn, viewer.guild_id, public)?,
        None => event.channel_id,
    };
    if channel_id != event.channel_id && !unposted {
        return Err(OpError::message("A posted event stays in its channel."));
    }
    if patch.apply_series && event.series_id.is_none() {
        return Err(OpError::message("This event isn't part of a series."));
    }
    if patch.repeat.is_some() && event.series_id.is_some() {
        return Err(OpError::message("This date already belongs to a series. Change the repeat there."));
    }
    let tz = zone(conn, viewer.guild_id)?;
    let starts_at = match patch.starts_local {
        Some(local) => local_instant(local, tz)?,
        None => event.starts_at,
    };
    let cutoff_at = match patch.cutoff {
        Some(dto::Cutoff::Never) => None,
        Some(dto::Cutoff::Before { minutes }) if minutes >= 0 => Some(starts_at - Duration::minutes(minutes)),
        Some(dto::Cutoff::Before { .. }) => {
            return Err(OpError::message("Signups have to close before the event starts."))
        }
        None => event.cutoff_at.map(|cutoff| cutoff + (starts_at - event.starts_at)),
    };
    let options = store::options_for(conn, &event)?
        .into_iter()
        .map(|opt| OptionInput {
            id: Some(opt.id),
            type_id: opt.type_id,
            capacity: opt.capacity,
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
        None => store::reminders_for(conn, &event)?
            .into_iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message,
            })
            .collect(),
    };
    let mention_role_ids = match &patch.mention_role_ids {
        Some(public) => ids::public::role_ids(conn, viewer.guild_id, public)?,
        None => store::event_role_ids(conn, &event, "mention")?,
    };
    let restrict_role_ids = match &patch.restrict_role_ids {
        Some(public) => ids::public::role_ids(conn, viewer.guild_id, public)?,
        None => store::event_role_ids(conn, &event, "restrict")?,
    };
    let attendee_role_id = match &patch.attendee_role_id {
        Some(public) => ids::public::role_id(conn, viewer.guild_id, public.as_deref())?,
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
        location: patch.location.clone().unwrap_or_else(|| event.location.clone()),
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
                cadence: repeat.cadence.trim().to_string(),
                end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
                max_occurrences: repeat.count,
            }),
            None => None,
        },
        action: match &posting {
            Some(Posting::Now) => logic::SaveAction::Publish,
            Some(Posting::At(_)) => logic::SaveAction::Schedule,
            Some(Posting::Hold) => logic::SaveAction::Draft,
            None if event.status == "draft" => logic::SaveAction::Draft,
            None => logic::SaveAction::Save,
        },
        post_at: match posting {
            Some(Posting::At(local)) => Some(local_instant(local, tz)?),
            _ => event.post_at,
        },
        apply_series: patch.apply_series,
    };
    logic::save_event(conn, input)?;
    Ok(Outcome::ok().synced(event.id))
}

pub fn delete_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let gone = matches!(event.status.as_str(), "draft" | "scheduled" | "failed");
    logic::delete_event(conn, event.id, viewer.membership_id)?;
    let location = if gone {
        path(conn, viewer, "")?
    } else {
        path(conn, viewer, &format!("/events/{public_id}"))?
    };
    Ok(Outcome::ok().at(location).synced(event.id))
}

pub fn archive_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    logic::archive_event(conn, event.id, viewer.membership_id)?;
    Ok(Outcome::ok().synced(event.id))
}

pub fn duplicate_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let copy = logic::duplicate_event(conn, event.id, viewer.membership_id)?;
    let copy = store::event(conn, copy)?.ok_or(OpError::NotFound)?;
    let location = path(conn, viewer, &format!("/events/{}", copy.public_id))?;
    Ok(Outcome::ok().at(location))
}

pub fn skip_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    logic::skip_event(conn, event.id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

fn series_dto(conn: &mut Connection, series: store::Series) -> Result<dto::Series, OpError> {
    let template = logic::series_template(conn, &series)?;
    let events = store::events_in_series(conn, series.id)?;
    let next = events
        .iter()
        .find(|event| matches!(event.status.as_str(), "posted" | "scheduled" | "draft"))
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

pub fn series_list(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Series>, OpError> {
    let mut rows = Vec::new();
    for series in store::list_series(conn, viewer.guild_id)? {
        if series.cadence != "once" {
            rows.push(series_dto(conn, series)?);
        }
    }
    Ok(rows)
}

pub fn series(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Series, OpError> {
    let series = series_here(conn, viewer, public_id)?;
    series_dto(conn, series)
}

pub fn patch_series(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    repeat: &dto::Repeat,
) -> Result<Outcome, OpError> {
    let series = series_here(conn, viewer, public_id)?;
    let tz = zone(conn, viewer.guild_id)?;
    logic::save_series_repeat(
        conn,
        series.id,
        viewer.membership_id,
        RepeatSpec {
            cadence: repeat.cadence.trim().to_string(),
            end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
            max_occurrences: repeat.count,
        },
    )?;
    Ok(Outcome::ok())
}

pub fn end_series(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let series = series_here(conn, viewer, public_id)?;
    logic::end_series(conn, series.id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

/// Options from a request, with their public ids turned into keys. An id that
/// isn't one of this event's options counts as a new option; a type has to be
/// in this server's library.
fn option_inputs(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    options: &[dto::SignupOption],
) -> Result<Vec<OptionInput>, OpError> {
    let known = match event_id {
        Some(event_id) => ids::public::option_ids(conn, event_id)?,
        None => Default::default(),
    };
    let types: HashMap<String, i64> = store::list_library(conn, guild_id)?
        .into_iter()
        .map(|kind| (kind.public_id.to_string(), kind.id))
        .collect();
    options
        .iter()
        .map(|opt| {
            let type_id = *types
                .get(opt.type_id.trim())
                .ok_or_else(|| OpError::message("That attendance type isn't in this server's library."))?;
            Ok(OptionInput {
                id: opt.id.as_ref().and_then(|id| known.get(id).copied()),
                type_id,
                capacity: opt.capacity,
                role_id: ids::public::role_id(conn, guild_id, opt.role_id.as_deref())?,
            })
        })
        .collect()
}

/// A preset's or template's type as the page shows it: the library type's name,
/// emoji, and attending, with this use's capacity and role.
fn option_dto(
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
        role_id: ids::public::role(conn, opt.role_id)?,
    })
}

fn library_by_id(conn: &mut Connection, guild_id: i64) -> Result<HashMap<i64, store::LibraryOption>, OpError> {
    Ok(store::list_library(conn, guild_id)?
        .into_iter()
        .map(|kind| (kind.id, kind))
        .collect())
}

pub fn presets(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Preset>, OpError> {
    let library = library_by_id(conn, viewer.guild_id)?;
    logic::presets(conn, viewer.guild_id)?
        .into_iter()
        .map(|(preset, types, default)| {
            Ok(dto::Preset {
                id: preset.public_id.to_string(),
                name: preset.name,
                multiple_signups: preset.multiple_signups,
                default,
                options: types
                    .iter()
                    .map(|opt| option_dto(conn, &library, opt))
                    .collect::<Result<_, OpError>>()?,
            })
        })
        .collect()
}

pub fn preset(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Preset, OpError> {
    let preset = preset_here(conn, viewer, public_id)?;
    presets(conn, viewer)?
        .into_iter()
        .find(|shown| shown.id == preset.public_id.to_string())
        .ok_or(OpError::NotFound)
}

fn preset_here(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<store::Preset, OpError> {
    store::preset_by_public(conn, public_id)?
        .filter(|preset| preset.guild_id == viewer.guild_id)
        .ok_or(OpError::NotFound)
}

fn preset_input(conn: &mut Connection, viewer: &Viewer, draft: &dto::PresetDraft) -> Result<logic::PresetInput, OpError> {
    Ok(logic::PresetInput {
        name: draft.name.clone(),
        multiple_signups: draft.multiple_signups,
        options: option_inputs(conn, viewer.guild_id, None, &draft.options)?,
    })
}

pub fn create_preset(conn: &mut Connection, viewer: &Viewer, draft: &dto::PresetDraft) -> Result<Outcome, OpError> {
    let input = preset_input(conn, viewer, draft)?;
    logic::save_preset(conn, viewer.guild_id, viewer.membership_id, None, &input)?;
    Ok(Outcome::ok())
}

pub fn update_preset(conn: &mut Connection, viewer: &Viewer, public_id: &str, draft: &dto::PresetDraft) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id)?;
    let input = preset_input(conn, viewer, draft)?;
    logic::save_preset(conn, viewer.guild_id, viewer.membership_id, Some(preset.id), &input)?;
    Ok(Outcome::ok())
}

pub fn default_preset(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id)?;
    logic::set_default_preset(conn, preset.id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

pub fn delete_preset(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let preset = preset_here(conn, viewer, public_id)?;
    logic::delete_preset(conn, preset.id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

/// Gives an event a preset's attendance; responses on types it keeps stay.
pub fn apply_preset(conn: &mut Connection, viewer: &Viewer, public_id: &str, body: &dto::ApplyPreset) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let preset = preset_here(conn, viewer, &body.preset)?;
    logic::apply_preset(conn, event.id, viewer.membership_id, preset.id)?;
    Ok(Outcome::ok().synced(event.id))
}

pub fn library(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::LibraryItem>, OpError> {
    let usage = store::type_usage(conn, viewer.guild_id)?;
    Ok(store::list_library(conn, viewer.guild_id)?
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
pub fn add_library(conn: &mut Connection, viewer: &Viewer, draft: &dto::TypeDraft) -> Result<dto::LibraryItem, OpError> {
    let made = logic::add_type(conn, viewer.guild_id, viewer.membership_id, &type_input(draft))?;
    Ok(dto::LibraryItem {
        id: made.public_id.to_string(),
        name: made.name,
        emoji: made.emoji,
        attending: made.attending,
        used_by: String::new(),
    })
}

fn type_here(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<store::LibraryOption, OpError> {
    store::library_type_by_public(conn, viewer.guild_id, public_id)?.ok_or(OpError::NotFound)
}

/// Renames a type, or changes its emoji or attending, everywhere it is used.
/// Posted events that use it are refreshed on Discord.
pub fn update_library(conn: &mut Connection, viewer: &Viewer, public_id: &str, draft: &dto::TypeDraft) -> Result<Outcome, OpError> {
    let kind = type_here(conn, viewer, public_id)?;
    let posted = logic::update_type(conn, kind.id, viewer.membership_id, &type_input(draft))?;
    Ok(Outcome {
        jobs: posted.into_iter().map(DiscordJob::sync).collect(),
        ..Outcome::ok()
    })
}

pub fn delete_library(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let kind = type_here(conn, viewer, public_id)?;
    logic::delete_type(conn, kind.id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

pub fn commands(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::CommandLog>, OpError> {
    let people = store::list_memberships(conn, viewer.guild_id)?;
    Ok(store::list_commands(conn, viewer.guild_id, 30)?
        .into_iter()
        .rev()
        .map(|row| dto::CommandLog {
            user: people
                .iter()
                .find(|p| row.membership_id == Some(p.id))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "Marian".into()),
            line: row.line,
            reply: row.reply,
        })
        .collect())
}

pub fn routines(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::RoutineSummary>, OpError> {
    logic::list_routines(conn, viewer.guild_id)
}

pub fn routine(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::RoutineView, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    logic::routine_page_data(conn, id)?.ok_or(OpError::NotFound)
}

/// A routine form's channel and event details, with public ids turned into keys.
fn routine_parts(
    conn: &mut Connection,
    viewer: &Viewer,
    draft: &dto::RoutineDraft,
) -> Result<(i64, Option<logic::EventExtras>), OpError> {
    let channel_id = ids::public::channel_id(conn, viewer.guild_id, &draft.channel_id)?;
    let extras = draft
        .event
        .as_ref()
        .map(|event| logic::routine_event_from(conn, viewer.guild_id, event))
        .transpose()?;
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

pub fn create_routine(conn: &mut Connection, viewer: &Viewer, draft: &dto::RoutineDraft) -> Result<Outcome, OpError> {
    let (channel_id, extras) = routine_parts(conn, viewer, draft)?;
    let (_id, public_id, jobs) = logic::create_routine(
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
    )?;
    let location = path(conn, viewer, &format!("/routines/{public_id}"))?;
    Ok(Outcome {
        jobs,
        ..Outcome::ok().at(location)
    })
}

pub fn update_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::RoutineDraft,
) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    let (channel_id, extras) = routine_parts(conn, viewer, draft)?;
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
    )?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::ok()
    })
}

pub fn routine_days(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    days: &dto::RoutineDays,
) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    match days.scope {
        dto::DayScope::Usual => {
            logic::save_usual_days(conn, id, viewer.membership_id, &days.days)?;
            Ok(Outcome::ok())
        }
        dto::DayScope::Run => {
            let effect = logic::save_run_days(conn, id, viewer.membership_id, &days.days)?;
            Ok(Outcome {
                jobs: effect.jobs,
                ..Outcome::ok()
            })
        }
    }
}

pub fn vote(conn: &mut Connection, viewer: &Viewer, public_id: &str, vote: &dto::Vote) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    let run = logic::open_run_id(conn, id)?.ok_or_else(|| OpError::message("That poll is closed."))?;
    let effect = logic::cast_run(conn, run, viewer.membership_id, vote.day)?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::ok()
    })
}

pub fn stop_routine(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    logic::stop_routine(conn, id, viewer.membership_id)?;
    Ok(Outcome::ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;
    use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};

    // Library types the test server has.
    const YES: i64 = 201;
    const TANK: i64 = 202;
    const MAYBE: i64 = 203;

    fn world() -> (Connection, Viewer) {
        let mut conn = store::open_memory().unwrap();
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
        .unwrap();
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

    /// Every id in an API answer is a UUIDv7 string; a row's sequential key never leaves.
    fn assert_public_ids(value: &serde_json::Value, path: &str) {
        let id_key = |key: &str| key == "id" || key.ends_with("_id") || key.ends_with("_ids") || key.starts_with("role_");
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
                            let text = id.as_str().unwrap_or_else(|| panic!("{here} is {id}, not a public id"));
                            let parsed = uuid::Uuid::parse_str(text).unwrap_or_else(|_| panic!("{here} is {text:?}"));
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

    #[test]
    fn the_api_names_rows_only_by_public_id() {
        let (mut conn, viewer) = world();
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
                options: vec![OptionInput { capacity: Some(2), role_id: Some(1), ..OptionInput::of_type(TANK) }],
                reminders: vec![],
                repeat: None,
                action: logic::SaveAction::Publish,
                post_at: None,
                apply_series: false,
            },
        )
        .unwrap();
        let public = store::event(&mut conn, id).unwrap().unwrap().public_id.to_string();
        let option = signups(&mut conn, &viewer, &public).unwrap().options[0].id.clone();
        let june = members(&mut conn, &viewer).unwrap().into_iter().find(|m| m.name == "June").unwrap().id;
        set_signup(&mut conn, &viewer, &public, &june, &option, true).unwrap();
        let mut next = settings(&mut conn, &viewer).unwrap();
        next.role_event = roles(&mut conn, &viewer).unwrap().into_iter().map(|role| role.id).collect();
        put_settings(&mut conn, &viewer, &next).unwrap();

        let answers = [
            ("event", serde_json::to_value(event(&mut conn, &viewer, &public).unwrap()).unwrap()),
            ("signups", serde_json::to_value(signups(&mut conn, &viewer, &public).unwrap()).unwrap()),
            ("settings", serde_json::to_value(settings(&mut conn, &viewer).unwrap()).unwrap()),
            ("roles", serde_json::to_value(roles(&mut conn, &viewer).unwrap()).unwrap()),
            ("members", serde_json::to_value(members(&mut conn, &viewer).unwrap()).unwrap()),
            ("channels", serde_json::to_value(channels(&mut conn, &viewer).unwrap()).unwrap()),
            ("membership", serde_json::to_value(membership(&mut conn, &viewer).unwrap()).unwrap()),
        ];
        for (name, answer) in &answers {
            assert_public_ids(answer, name);
        }
        let shown = answers[1].1.to_string();
        assert!(shown.contains(&june), "June shows by her public id");
        assert_eq!(answers[2].1["role_event"].as_array().unwrap().len(), 1, "settings keep the role");
    }

    #[test]
    fn an_id_from_another_server_is_not_found() {
        let (mut conn, viewer) = world();
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
        .unwrap();
        let their_role = ids::public::roles(&mut conn, &[9]).unwrap().remove(0);
        let their_channel = ids::public::channel(&mut conn, 9).unwrap();
        assert!(ids::public::role_ids(&mut conn, 1, &[their_role]).is_err());
        assert!(ids::public::channel_id(&mut conn, 1, &their_channel).is_err());
        let their_member: String = crate::schema::memberships::table
            .find(9)
            .select(crate::schema::memberships::public_id)
            .first::<uuid::Uuid>(&mut conn)
            .unwrap()
            .to_string();
        assert!(matches!(ids::public::member_id(&mut conn, viewer.guild_id, &their_member), Err(OpError::NotFound)));
        assert!(matches!(ids::public::member_id(&mut conn, viewer.guild_id, "9"), Err(OpError::NotFound)), "a row key is not an id here");
    }

    #[test]
    fn a_server_you_cannot_open_reads_as_one_that_does_not_exist() {
        let (mut conn, viewer) = world();
        conn.batch_execute("INSERT INTO guilds (id, name, timezone, discord_id) VALUES (2, 'Other', 'UTC', '77');")
            .unwrap();
        let ask = |conn: &mut Connection, server: &str| {
            let mut viewer = viewer.clone();
            bind_server(conn, &mut viewer, server)
        };
        assert!(matches!(ask(&mut conn, "77"), Err(OpError::NotFound)), "there, but not theirs");
        assert!(matches!(ask(&mut conn, "78"), Err(OpError::NotFound)), "not there at all");
        assert!(ask(&mut conn, "99").is_ok(), "their own");
    }

    #[test]
    fn an_event_lists_only_its_own_activity_newest_first() {
        let (mut conn, viewer) = world();
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
        .unwrap();
        let public = |conn: &mut Connection, id: i64| store::event(conn, id).unwrap().unwrap().public_id.to_string();
        let raid = public(&mut conn, 11);
        let bodies: Vec<String> = event_activity(&mut conn, &viewer, &raid, 1, 25)
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.body)
            .collect();
        assert_eq!(bodies, ["June signed up for Raid", "Nell created Raid"]);
        assert_eq!(activity(&mut conn, &viewer, 1, 25).unwrap().total, 4, "the server's log keeps everything");
        let theirs = public(&mut conn, 13);
        assert!(matches!(event_activity(&mut conn, &viewer, &theirs, 1, 25), Err(OpError::NotFound)));
    }

    #[test]
    fn the_event_list_filters_by_status_end_and_day() {
        let (mut conn, viewer) = world();
        let now = Utc::now();
        let mut add = |title: &str, status: &str, starts_at: DateTime<Utc>| {
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
            .unwrap();
            diesel::update(crate::schema::events::table.find(id))
                .set((crate::schema::events::status.eq(status), crate::schema::events::starts_at.eq(starts_at)))
                .execute(&mut conn)
                .unwrap();
        };
        add("old raid", "archived", now - Duration::days(20));
        add("last raid", "posted", now - Duration::days(2));
        add("next raid", "posted", now + Duration::days(2));
        add("plan", "draft", now + Duration::days(5));
        add("queued", "scheduled", now + Duration::days(9));
        let titles = |conn: &mut Connection, query: dto::EventQuery| -> Vec<String> {
            events(conn, &viewer, &dto::EventQuery { view: dto::EventView::All, ..query })
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
        assert_eq!(titles(&mut conn, upcoming), ["next raid", "queued"]);
        let drafts = dto::EventQuery { status: "draft".into(), ..Default::default() };
        assert_eq!(titles(&mut conn, drafts), ["plan"]);
        let past = dto::EventQuery { when: Some(dto::EventWhen::Past), ..Default::default() };
        assert_eq!(titles(&mut conn, past), ["last raid", "old raid"], "newest first");
        let warsaw: Tz = "Europe/Warsaw".parse().unwrap();
        let day = |at: DateTime<Utc>| at.with_timezone(&warsaw).date_naive();
        let days = dto::EventQuery {
            from_day: Some(day(now + Duration::days(2))),
            to_day: Some(day(now + Duration::days(5))),
            ..Default::default()
        };
        assert_eq!(titles(&mut conn, days), ["next raid", "plan"], "both days count");
    }

    #[test]
    fn a_patch_changes_only_what_it_names_and_keeps_the_event_posted() {
        let (mut conn, viewer) = world();
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
        .unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        let option = store::options_for(&mut conn, &event).unwrap()[0].id;
        logic::signup(&mut conn, id, option, 2).unwrap();
        let public = event.public_id.to_string();

        let moved = event.starts_at + Duration::hours(2);
        let local = moved.with_timezone(&chrono_tz::Europe::Warsaw).naive_local();
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
        .unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!(after.title, "Raid night");
        assert_eq!(after.description, "Bring food");
        assert_eq!(after.location, "Keep");
        assert_eq!(after.status, "posted");
        assert!(after.multiple_signups);
        assert_eq!(after.starts_at.timestamp(), moved.timestamp());
        let cutoff = after.cutoff_at.unwrap();
        assert_eq!((after.starts_at - cutoff).num_minutes(), 60, "the cutoff moves with the start");
        assert_eq!(store::options_for(&mut conn, &after).unwrap()[0].id, option);
        assert_eq!(store::signups_for(&mut conn, &after).unwrap().len(), 1);

        diesel::update(crate::schema::events::table.filter(crate::schema::events::id.eq(id)))
            .set(crate::schema::events::signups_closed.eq(true))
            .execute(&mut conn)
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
        .unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
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
                cutoff: Some(dto::Cutoff::Before { minutes: (moved - Utc::now()).num_minutes() + 24 * 60 }),
                ..Default::default()
            },
        )
        .unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
        assert!(after.signups_closed, "a cutoff already past keeps them closed");

        // The API names roles and channels by public id, never the row key.
        let role = ids::public::roles(&mut conn, &[1]).unwrap().remove(0);
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
        .unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
        let reminders = store::reminders_for(&mut conn, &after).unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!((reminders[0].offset_minutes, reminders[0].message.as_str()), (120, "Bring flasks"));
        assert!(after.mention_everyone);
        assert_eq!(store::event_role_ids(&mut conn, &after, "mention").unwrap(), [1]);
        assert_eq!(store::event_role_ids(&mut conn, &after, "restrict").unwrap(), [1]);
        assert_eq!(after.attendee_role_id, Some(1));

        let cleared: dto::EventPatch = serde_json::from_str(r#"{"attendee_role_id":null}"#).unwrap();
        assert_eq!(cleared.attendee_role_id, Some(None));
        patch_event(&mut conn, &viewer, &public, &cleared).unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!(after.attendee_role_id, None);
        assert_eq!(store::event_role_ids(&mut conn, &after, "mention").unwrap(), [1], "unset fields stay");

        patch_event(
            &mut conn,
            &viewer,
            &public,
            &dto::EventPatch {
                reminders: Some(Vec::new()),
                ..Default::default()
            },
        )
        .unwrap();
        let after = store::event(&mut conn, id).unwrap().unwrap();
        assert!(store::reminders_for(&mut conn, &after).unwrap().is_empty(), "reminders can be cleared");

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
            .unwrap();
            assert_eq!(store::event(&mut conn, id).unwrap().unwrap().location, expected);
        }

        let updates: i64 = crate::schema::activity::table
            .filter(crate::schema::activity::body.eq("Updated Raid night."))
            .count()
            .get_result(&mut conn)
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
        .unwrap_err();
        assert!(err.to_string().contains("title"));
    }

    #[test]
    fn a_draft_can_be_scheduled_or_posted_from_its_page() {
        let (mut conn, viewer) = world();
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
        .unwrap();
        let public = store::event(&mut conn, id).unwrap().unwrap().public_id.to_string();
        let local = |at: DateTime<Utc>| at.with_timezone(&chrono_tz::Europe::Warsaw).naive_local();

        let past = dto::PostAt { post_local: local(Utc::now() - Duration::hours(1)) };
        let err = schedule_event(&mut conn, &viewer, &public, &past).unwrap_err().to_string();
        assert!(err.contains("past"), "{err}");

        let post_at = starts_at - Duration::days(1);
        schedule_event(&mut conn, &viewer, &public, &dto::PostAt { post_local: local(post_at) }).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!(event.status, "scheduled");
        assert_eq!(event.post_at.unwrap().timestamp() / 60, post_at.timestamp() / 60);

        unschedule_event(&mut conn, &viewer, &public).unwrap();
        let event = store::event(&mut conn, id).unwrap().unwrap();
        assert_eq!((event.status.as_str(), event.post_at), ("draft", None), "back to a draft");

        // Before posting, an option can go.
        let mut options = store::options_for(&mut conn, &event).unwrap();
        assert_eq!(options.len(), 1);
        let extra = OptionInput::of_type(MAYBE);
        let keep = OptionInput { id: Some(options[0].id), ..OptionInput::of_type(options[0].type_id) };
        logic::save_options(&mut conn, id, 1, &[keep.clone(), extra]).unwrap();
        options = store::options_for(&mut conn, &event).unwrap();
        assert_eq!(options.len(), 2);
        logic::save_options(&mut conn, id, 1, &[keep]).unwrap();
        assert_eq!(store::options_for(&mut conn, &event).unwrap().len(), 1);

        publish_event(&mut conn, &viewer, &public).unwrap();
        assert_eq!(store::event(&mut conn, id).unwrap().unwrap().status, "posted");
        let moved = dto::EventPatch { channel_id: Some(ids::public::channel(&mut conn, 2).unwrap()), ..Default::default() };
        let err = patch_event(&mut conn, &viewer, &public, &moved).unwrap_err().to_string();
        assert!(err.contains("stays in its channel"), "{err}");
        let err = publish_event(&mut conn, &viewer, &public).unwrap_err().to_string();
        assert!(err.contains("already gone out"), "{err}");
        let err = schedule_event(&mut conn, &viewer, &public, &dto::PostAt { post_local: local(post_at) })
            .unwrap_err()
            .to_string();
        assert!(err.contains("already gone out"), "{err}");
    }

    #[test]
    fn an_event_starts_repeating_from_its_page() {
        let (mut conn, viewer) = world();
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
        .unwrap();
        let public = store::event(&mut conn, id).unwrap().unwrap().public_id.to_string();
        assert!(event(&mut conn, &viewer, &public).unwrap().series.is_none());

        let weekly = dto::EventPatch {
            repeat: Some(dto::Repeat { cadence: "weekly".into(), count: Some(3), until: None }),
            ..Default::default()
        };
        patch_event(&mut conn, &viewer, &public, &weekly).unwrap();
        let shown = event(&mut conn, &viewer, &public).unwrap();
        let series = shown.series.expect("the event now belongs to a series");
        assert_eq!(series.cadence, "weekly");
        assert_eq!(series.max_occurrences, Some(3));
        assert!(series.active);
        assert_eq!(shown.status, "posted");

        let err = patch_event(&mut conn, &viewer, &public, &weekly).unwrap_err().to_string();
        assert!(err.contains("already belongs to a series"), "{err}");
    }
}
