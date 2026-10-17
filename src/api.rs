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
use crate::jobs::DiscordJob;
use crate::logic::{self, EventInput, OptionInput, PresetBody, ReminderInput, RepeatSpec};
use crate::store::{self, Connection};

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
    /// An event whose Discord post and live pages need refreshing.
    pub sync: Option<i64>,
    pub jobs: Vec<DiscordJob>,
}

impl Outcome {
    fn notice(notice: impl Into<String>) -> Self {
        Self {
            done: dto::Done {
                notice: notice.into(),
                location: None,
            },
            ..Self::default()
        }
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

/// Points the viewer at the server with this Discord id, remembering it as their last.
pub fn bind_server(conn: &mut Connection, viewer: &mut Viewer, server: &str) -> Result<(), OpError> {
    let guild = store::guild_by_discord(conn, server)?.ok_or(OpError::NotFound)?;
    bind_guild(conn, viewer, guild.id)
}

pub fn bind_guild(conn: &mut Connection, viewer: &mut Viewer, guild_id: i64) -> Result<(), OpError> {
    if guild_id <= 0 || !viewer.allowed.contains(&guild_id) {
        return Err(OpError::message("That server isn't one you signed into."));
    }
    if store::guild(conn, guild_id)?.is_none() {
        return Err(OpError::NotFound);
    }
    let user = logic::ensure_discord_user(
        conn,
        guild_id,
        &viewer.discord_user_id,
        &viewer.display_name,
    )?;
    if viewer.guild_id != guild_id || viewer.membership_id != user.id {
        store::move_web_session(conn, &viewer.token, guild_id, user.id)?;
    }
    viewer.guild_id = guild_id;
    viewer.membership_id = user.id;
    Ok(())
}

/// The server to open first: the last one used, then the first one signed into.
pub fn home_server(conn: &mut Connection, viewer: &Viewer) -> Option<String> {
    std::iter::once(viewer.guild_id)
        .chain(viewer.allowed.iter().copied())
        .filter(|id| *id > 0 && viewer.allowed.contains(id))
        .find_map(|id| store::discord_server_id(conn, id).ok().filter(|s| !s.is_empty()))
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
        last_server: home_server(conn, viewer),
    })
}

pub fn server(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Server, OpError> {
    let guild = store::guild(conn, viewer.guild_id)?.ok_or(OpError::NotFound)?;
    let settings = store::settings(conn, viewer.guild_id)?;
    let counts = dto::Counts {
        drafts: store::count_status(conn, viewer.guild_id, "draft")?,
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
        id: user.id,
        name: user.name,
        organizer: user.organizer,
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
    let mut notices = Vec::new();
    if let Some(enabled) = patch.reminders_enabled {
        notices.push(logic::set_user_reminders(conn, viewer.membership_id, enabled)?);
    }
    if let Some(enabled) = patch.google_auto_sync {
        notices.push(crate::commands::google_auto_sync(
            conn,
            viewer.guild_id,
            viewer.membership_id,
            enabled,
        )?);
    }
    Ok(Outcome::notice(notices.join(" ")))
}

pub fn settings(conn: &mut Connection, viewer: &Viewer) -> Result<dto::Settings, OpError> {
    let guild = store::guild(conn, viewer.guild_id)?.ok_or(OpError::NotFound)?;
    let s = store::settings(conn, viewer.guild_id)?;
    Ok(dto::Settings {
        timezone: guild.timezone,
        default_reminder_minutes: s.default_reminder_minutes,
        auto_archive_hours: s.auto_archive_hours,
        post_lead_hours: s.post_lead_hours,
        digest: s.digest,
        role_event: s.role_event,
        role_edit: s.role_edit,
        role_delete: s.role_delete,
        role_channel: s.role_channel,
        role_signup: s.role_signup,
        thread_reminders: s.thread_reminders,
        auto_archive: s.auto_archive,
        event_threads: s.event_threads,
        auto_join_threads: s.auto_join_threads,
        show_attendees: s.show_attendees,
        hour_24: s.hour_24,
        localize_time: s.localize_time,
        purge_channels: s.purge_channels,
        temp_roles: s.temp_roles,
        calendar_auto_sync: s.calendar_auto_sync,
    })
}

pub fn put_settings(
    conn: &mut Connection,
    viewer: &Viewer,
    next: &dto::Settings,
) -> Result<Outcome, OpError> {
    let user = must_member(conn, viewer)?;
    if !user.organizer {
        return Err(OpError::message(
            "Only someone who runs the server can change settings.",
        ));
    }
    clock::zone(&next.timezone)
        .map_err(|_| OpError::message("That timezone isn't one Marian knows."))?;
    let mut s = store::settings(conn, viewer.guild_id)?;
    s.default_reminder_minutes = bounded("The default reminder", next.default_reminder_minutes, 1, 10080)?;
    s.auto_archive_hours = bounded("Archive after", next.auto_archive_hours, 1, 168)?;
    s.post_lead_hours = bounded("Post lead", next.post_lead_hours, 0, 336)?;
    s.digest = match next.digest.as_str() {
        "daily" | "weekly" => next.digest.clone(),
        _ => "off".into(),
    };
    let roles: Vec<i64> = store::list_roles(conn, viewer.guild_id)?
        .into_iter()
        .map(|role| role.id)
        .collect();
    let known = |id: Option<i64>| id.filter(|id| roles.contains(id));
    s.role_event = known(next.role_event);
    s.role_edit = known(next.role_edit);
    s.role_delete = known(next.role_delete);
    s.role_channel = known(next.role_channel);
    s.role_signup = known(next.role_signup);
    s.thread_reminders = next.thread_reminders;
    s.auto_archive = next.auto_archive;
    s.event_threads = next.event_threads;
    s.auto_join_threads = next.auto_join_threads;
    s.show_attendees = next.show_attendees;
    s.hour_24 = next.hour_24;
    s.localize_time = next.localize_time;
    s.purge_channels = next.purge_channels;
    s.temp_roles = next.temp_roles;
    s.calendar_auto_sync = next.calendar_auto_sync;
    store::set_guild_timezone(conn, viewer.guild_id, &next.timezone)?;
    store::update_settings(conn, &s)?;
    Ok(Outcome::notice("Settings saved."))
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
            id: role.id,
            name: role.name,
        })
        .collect())
}

pub fn members(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Member>, OpError> {
    Ok(store::list_memberships(conn, viewer.guild_id)?
        .into_iter()
        .map(|person| dto::Member {
            id: person.id,
            name: person.name,
            color: person.color,
        })
        .collect())
}

pub fn channels(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Channel>, OpError> {
    let events = store::list_events(conn, viewer.guild_id)?;
    Ok(store::list_channels(conn, viewer.guild_id)?
        .into_iter()
        .map(|channel| dto::Channel {
            events: events.iter().filter(|e| e.channel_id == channel.id).count() as i64,
            id: channel.id,
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
    let events: Vec<store::Event> = match series_id {
        Some(id) => store::events_in_series(conn, id)?,
        None => store::list_events(conn, viewer.guild_id)?,
    }
    .into_iter()
    .filter(|event| series_id.is_some() || matches_view(query.view, event, now))
    .filter(|event| query.from.is_none_or(|from| event.starts_at >= from))
    .filter(|event| query.to.is_none_or(|to| event.starts_at < to))
    .filter(|event| q.is_empty() || event.title.to_lowercase().contains(&q))
    .collect();
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
        Some(id) => store::series(conn, id)?.map(|series| dto::SeriesRef {
            id: series.public_id.to_string(),
            cadence: series.cadence,
        }),
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
        channel_id: event.channel_id,
        channel: channel_name.clone(),
        attendee_role_id: event.attendee_role_id,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, &event, "mention")?,
        restrict_role_ids: store::event_role_ids(conn, &event, "restrict")?,
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

pub fn event_options(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
) -> Result<Vec<dto::SignupOption>, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    Ok(store::options_for(conn, &event)?
        .into_iter()
        .map(|opt| dto::SignupOption {
            id: Some(opt.id),
            name: opt.name,
            emoji: opt.emoji,
            capacity: opt.capacity,
            waitlist: opt.waitlist,
            role_id: opt.role_id,
            reminders: opt.reminders_enabled,
        })
        .collect())
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

pub fn thread(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Thread, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let people = store::list_memberships(conn, viewer.guild_id)?;
    let members = store::thread_member_ids(conn, &event)?
        .into_iter()
        .filter_map(|id| people.iter().find(|p| p.id == id).map(|p| p.name.clone()))
        .collect();
    let messages = store::thread_messages(conn, &event)?
        .into_iter()
        .map(|note| dto::ThreadNote {
            body: note.body,
            place: note.place,
            created_at: note.created_at,
        })
        .collect();
    Ok(dto::Thread { messages, members })
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
                id: p.id,
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
            };
            let (open, reason, waitlist_next) = gate.button(opt, mine, confirmed);
            dto::OptionSignups {
                id: opt.id,
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                role: opt
                    .role_id
                    .and_then(|id| role_rows.iter().find(|r| r.id == id))
                    .map(|r| r.name.clone()),
                reminders: opt.reminders_enabled,
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
        notes: signup_notes(&event, &options, &signups, &user, &roles, &restrict_ids, &role_rows, now),
        show_names,
        can_manage,
    })
}

struct Gate<'a> {
    event: &'a store::Event,
    user: &'a store::Membership,
    roles: &'a [i64],
    restrict_ids: &'a [i64],
    closed: bool,
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
            && !self.user.organizer
            && !self.restrict_ids.iter().any(|id| self.roles.contains(id))
        {
            return (false, "This event is limited to another role.".into(), false);
        }
        if let Some(role_id) = opt.role_id {
            if !self.user.organizer && !self.roles.contains(&role_id) {
                return (false, "Your roles don't include this option.".into(), false);
            }
        }
        if let Some(cap) = opt.capacity {
            if confirmed as i64 >= cap {
                if opt.waitlist {
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
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if event.status != "posted" {
        notes.push("Signups open once the event is posted.".to_string());
    }
    if event.signups_closed || cutoff_passed(event, now) {
        notes.push("Signups are closed.".into());
    }
    if !restrict_ids.is_empty() && !user.organizer && !restrict_ids.iter().any(|id| roles.contains(id)) {
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
            if !user.organizer && !roles.contains(&role_id) {
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
            if confirmed >= cap && !opt.waitlist {
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
    option_id: i64,
    on: bool,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    if signed_up(conn, &event, option_id, viewer.membership_id)? == on {
        return Ok(Outcome::notice(""));
    }
    let notice = logic::signup(conn, event.id, option_id, viewer.membership_id)?;
    Ok(Outcome::notice(notice).synced(event.id))
}

/// An organizer adding someone to an option, or taking them off it.
pub fn set_signup(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    member_id: i64,
    option_id: i64,
    on: bool,
) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let notice = logic::organizer_response(
        conn,
        event.id,
        viewer.membership_id,
        member_id,
        option_id,
        if on { "add" } else { "remove" },
    )?;
    Ok(Outcome::notice(notice).synced(event.id))
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

pub fn activity(conn: &mut Connection, viewer: &Viewer, limit: i64) -> Result<Vec<dto::Activity>, OpError> {
    let mut rows = Vec::new();
    for item in store::list_activity(conn, viewer.guild_id, limit.clamp(1, 200))? {
        let event_id = match item.event_id {
            Some(id) => store::event(conn, id)?.map(|event| event.public_id.to_string()),
            None => None,
        };
        rows.push(dto::Activity {
            body: item.body,
            created_at: item.created_at,
            event_id,
        });
    }
    Ok(rows)
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
    Ok(EventInput {
        id: existing.map(|event| event.id),
        guild_id: viewer.guild_id,
        actor_id: viewer.membership_id,
        channel_id: draft.channel_id,
        title: draft.title.clone(),
        description: draft.description.clone(),
        location: draft.location.clone(),
        starts_at: local_instant(draft.starts_local, tz)?,
        duration_minutes: draft.duration_minutes,
        color: draft.color.clone(),
        image_url: None,
        cutoff_at: draft.cutoff_local.map(|dt| local_instant(dt, tz)).transpose()?,
        attendee_role_id: draft.attendee_role_id,
        thread_enabled: draft.thread_enabled,
        multiple_signups: draft.multiple_signups,
        mention_everyone: draft.mention_everyone,
        mention_role_ids: draft.mention_role_ids.clone(),
        restrict_role_ids: draft.restrict_role_ids.clone(),
        options: draft
            .options
            .iter()
            .filter(|opt| !opt.name.trim().is_empty())
            .take(8)
            .map(|opt| OptionInput {
                id: opt.id,
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                waitlist: opt.waitlist,
                role_id: opt.role_id,
                reminders_enabled: opt.reminders,
            })
            .collect(),
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
    Ok(Outcome::notice("Saved.").at(location).synced(event_id))
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

pub fn delete_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let gone = matches!(event.status.as_str(), "draft" | "scheduled" | "failed");
    let notice = logic::delete_event(conn, event.id, viewer.membership_id)?;
    let location = if gone {
        path(conn, viewer, "")?
    } else {
        path(conn, viewer, &format!("/events/{public_id}"))?
    };
    Ok(Outcome::notice(notice).at(location).synced(event.id))
}

pub fn archive_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let notice = logic::archive_event(conn, event.id, viewer.membership_id)?;
    Ok(Outcome::notice(notice).synced(event.id))
}

pub fn duplicate_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    let copy = logic::duplicate_event(conn, event.id, viewer.membership_id)?;
    let copy = store::event(conn, copy)?.ok_or(OpError::NotFound)?;
    let location = path(conn, viewer, &format!("/events/{}/edit", copy.public_id))?;
    Ok(Outcome::notice("A draft copy is ready, one week later.").at(location))
}

pub fn skip_event(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let event = event_here(conn, viewer, public_id)?;
    Ok(Outcome::notice(logic::skip_event(conn, event.id, viewer.membership_id)?))
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
    let notice = logic::save_series_repeat(
        conn,
        series.id,
        viewer.membership_id,
        RepeatSpec {
            cadence: repeat.cadence.trim().to_string(),
            end_on: repeat.until.map(|date| end_of_day(date, tz)).transpose()?,
            max_occurrences: repeat.count,
        },
    )?;
    Ok(Outcome::notice(notice))
}

pub fn end_series(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let series = series_here(conn, viewer, public_id)?;
    Ok(Outcome::notice(logic::end_series(conn, series.id, viewer.membership_id)?))
}

fn option_dto(opt: OptionInput) -> dto::SignupOption {
    dto::SignupOption {
        id: None,
        name: opt.name,
        emoji: opt.emoji,
        capacity: opt.capacity,
        waitlist: opt.waitlist,
        role_id: opt.role_id,
        reminders: opt.reminders_enabled,
    }
}

fn preset_dto(preset: store::Preset) -> dto::Preset {
    let body = serde_json::from_value::<PresetBody>(preset.body).unwrap_or(PresetBody {
        multiple_signups: false,
        options: Vec::new(),
    });
    dto::Preset {
        id: preset.public_id.to_string(),
        name: preset.name,
        multiple_signups: body.multiple_signups,
        options: body.options.into_iter().map(option_dto).collect(),
    }
}

pub fn presets(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::Preset>, OpError> {
    Ok(store::list_presets(conn, viewer.guild_id)?
        .into_iter()
        .map(preset_dto)
        .collect())
}

pub fn preset(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<dto::Preset, OpError> {
    let preset = store::preset_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if preset.guild_id != viewer.guild_id {
        return Err(OpError::NotFound);
    }
    Ok(preset_dto(preset))
}

/// The options a new event starts with.
pub fn default_options() -> Vec<dto::SignupOption> {
    dto::default_signup_options()
}

pub fn create_preset(conn: &mut Connection, viewer: &Viewer, draft: &dto::PresetDraft) -> Result<Outcome, OpError> {
    let library = store::list_library(conn, viewer.guild_id)?;
    let options = draft
        .options
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .map(|name| OptionInput {
            id: None,
            name: name.to_string(),
            emoji: library
                .iter()
                .find(|row| row.name.eq_ignore_ascii_case(name))
                .map(|row| row.emoji.clone())
                .unwrap_or_else(|| "•".into()),
            capacity: None,
            waitlist: false,
            role_id: None,
            reminders_enabled: true,
        })
        .collect();
    let body = PresetBody {
        multiple_signups: draft.multiple_signups,
        options,
    };
    logic::save_preset(conn, viewer.guild_id, viewer.membership_id, &draft.name, &body)?;
    Ok(Outcome::notice(format!("Saved the {} preset.", draft.name)))
}

pub fn delete_preset(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let preset = store::preset_by_public(conn, public_id)?.ok_or(OpError::NotFound)?;
    if preset.guild_id != viewer.guild_id {
        return Err(OpError::NotFound);
    }
    logic::delete_preset(conn, preset.id, viewer.membership_id)?;
    Ok(Outcome::notice("Removed the preset."))
}

pub fn library(conn: &mut Connection, viewer: &Viewer) -> Result<Vec<dto::LibraryItem>, OpError> {
    Ok(store::list_library(conn, viewer.guild_id)?
        .into_iter()
        .map(|item| dto::LibraryItem {
            name: item.name,
            emoji: item.emoji,
        })
        .collect())
}

pub fn add_library(conn: &mut Connection, viewer: &Viewer, item: &dto::LibraryItem) -> Result<Outcome, OpError> {
    logic::library_add(conn, viewer.guild_id, viewer.membership_id, &item.name, &item.emoji)?;
    Ok(Outcome::notice(format!("Added {} to the option library.", item.name)))
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
    let (_id, public_id, jobs) = logic::create_routine(
        conn,
        viewer.guild_id,
        viewer.membership_id,
        draft.channel_id,
        &draft.title,
        &draft.description,
        draft.duration_minutes,
        clock_minutes(&draft.start_time)?,
        weekday(draft.poll_weekday)?,
        clock_minutes(&draft.poll_time)?,
        weekday(draft.lock_weekday)?,
        clock_minutes(&draft.lock_time)?,
        &draft.days,
        Utc::now(),
    )?;
    let location = path(conn, viewer, &format!("/routines/{public_id}"))?;
    Ok(Outcome {
        jobs,
        ..Outcome::notice("Saved the routine.").at(location)
    })
}

pub fn update_routine(
    conn: &mut Connection,
    viewer: &Viewer,
    public_id: &str,
    draft: &dto::RoutineDraft,
) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    let effect = logic::save_routine(
        conn,
        id,
        viewer.membership_id,
        draft.channel_id,
        &draft.title,
        &draft.description,
        draft.duration_minutes,
        clock_minutes(&draft.start_time)?,
        weekday(draft.poll_weekday)?,
        clock_minutes(&draft.poll_time)?,
        weekday(draft.lock_weekday)?,
        clock_minutes(&draft.lock_time)?,
        &draft.days,
        Utc::now(),
    )?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::notice(effect.notice)
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
        dto::DayScope::Usual => Ok(Outcome::notice(logic::save_usual_days(
            conn,
            id,
            viewer.membership_id,
            &days.days,
        )?)),
        dto::DayScope::Run => {
            let effect = logic::save_run_days(conn, id, viewer.membership_id, &days.days)?;
            Ok(Outcome {
                jobs: effect.jobs,
                ..Outcome::notice(effect.notice)
            })
        }
    }
}

pub fn vote(conn: &mut Connection, viewer: &Viewer, public_id: &str, vote: &dto::Vote) -> Result<Outcome, OpError> {
    let view = routine(conn, viewer, public_id)?;
    let run = view
        .run
        .filter(|run| run.open)
        .ok_or_else(|| OpError::message("That poll is closed."))?;
    let effect = logic::cast_run(conn, run.id, viewer.membership_id, vote.day)?;
    Ok(Outcome {
        jobs: effect.jobs,
        ..Outcome::notice(effect.notice)
    })
}

pub fn stop_routine(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<Outcome, OpError> {
    let id = logic::routine_in_guild(conn, viewer.guild_id, public_id)?;
    Ok(Outcome::notice(logic::stop_routine(conn, id, viewer.membership_id)?))
}

/// Looks up the event id an SSE stream follows.
pub fn event_key(conn: &mut Connection, viewer: &Viewer, public_id: &str) -> Result<i64, OpError> {
    Ok(event_here(conn, viewer, public_id)?.id)
}
