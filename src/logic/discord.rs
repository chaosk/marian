use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::catalog::slug_channel;
use super::db::{must_channel, must_event, must_guild, trim_chars};
use super::model::*;
use crate::clock;
use crate::error::OpError;
use crate::i18n::{self, Lang};
use crate::schema;
use crate::tr;
use crate::store::{self, Channel, Connection, Membership, Role};

pub enum Attendance {
    Idle,
    Waiting,
    Ready(AttendanceWork),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendanceWork {
    pub title: String,
    pub guild_discord_id: String,
    pub channel_discord_id: String,
    pub message_id: String,
    pub thread_discord_id: Option<String>,
    /// The event has a thread turned on and none in Discord yet.
    pub open_thread: bool,
    pub thread_name: String,
    pub thread_add: Vec<String>,
    pub thread_remove: Vec<String>,
    pub role_id: Option<i64>,
    pub role_discord_id: Option<String>,
    pub role_name: String,
    pub role_color: u32,
    pub role_add: Vec<String>,
    pub role_remove: Vec<String>,
}

pub fn attendance(conn: &mut Connection, event_id: i64) -> Result<Attendance, OpError> {
    let event = must_event(conn, event_id)?;
    let guild = must_guild(conn, event.guild_id)?;
    let channel = must_channel(conn, event.channel_id)?;
    let Some(guild_discord_id) = guild.discord_id else {
        return Ok(Attendance::Idle);
    };
    let Some(channel_discord_id) = channel.discord_id else {
        return Ok(Attendance::Idle);
    };
    let Some(message_id) = event.discord_message_id.clone() else {
        // A posted event's message is on its way, so wait for it. One that
        // isn't posted (a draft, or a scheduled post) has no roles to give yet.
        return Ok(if event.status == "posted" { Attendance::Waiting } else { Attendance::Idle });
    };
    let settings = store::settings(conn, event.guild_id)?;
    let thread_discord_id = store::thread_discord_id(conn, event_id)?;
    let manage_thread = event.thread_enabled && settings.auto_join_threads;
    let desired_thread = if manage_thread {
        let members = store::thread_member_ids(conn, &event)?;
        store::discord_ids_for_memberships(conn, &members)?
    } else {
        Vec::new()
    };
    let applied_thread = store::applied_membership_ids(conn, event_id, "thread")?;
    let (thread_add, thread_remove) = id_diff(&desired_thread, &applied_thread);
    let grants = store::grant_membership_ids(conn, &event)?;
    let desired_role = store::discord_ids_for_memberships(conn, &grants)?;
    let applied_role = store::applied_membership_ids(conn, event_id, "role")?;
    let (role_add, role_remove) = id_diff(&desired_role, &applied_role);
    let role = event
        .attendee_role_id
        .and_then(|id| store::role(conn, id).ok().flatten());
    Ok(Attendance::Ready(AttendanceWork {
        title: event.title.clone(),
        guild_discord_id,
        channel_discord_id,
        message_id,
        open_thread: event.thread_enabled && thread_discord_id.is_none(),
        thread_discord_id,
        thread_name: event_thread_name(
            i18n::lang_of(conn, event.guild_id),
            &event.title,
            event.starts_at,
            &guild.timezone,
        ),
        thread_add,
        thread_remove,
        role_id: role.as_ref().map(|role| role.id),
        role_discord_id: role.as_ref().and_then(|role| role.discord_id.clone()),
        role_name: role
            .as_ref()
            .map(|role| trim_chars(&role.name, 100))
            .unwrap_or_default(),
        role_color: role
            .as_ref()
            .map(|role| role_color(&role.color))
            .unwrap_or(0x1e3a6e),
        role_add,
        role_remove,
    }))
}

/// Discord thread title: the event name, then its date in the guild's zone.
/// Discord allows 100 characters, so a long name is shortened and the date stays.
pub fn event_thread_name(lang: Lang, title: &str, starts_at: DateTime<Utc>, timezone: &str) -> String {
    let date = clock::zone(timezone)
        .ok()
        .map(|tz| i18n::format_day(lang, starts_at.with_timezone(&tz).date_naive()));
    let fallback = tr!(lang, "post-event-fallback");
    let title = title.trim();
    let title = if title.is_empty() { fallback.as_str() } else { title };
    let Some(date) = date else {
        return trim_chars(title, 100);
    };
    let suffix = format!(" -- {date}");
    let room = 100usize.saturating_sub(suffix.chars().count()).max(1);
    format!("{}{suffix}", trim_chars(title, room))
}

pub(super) fn stored_thread_name(
    conn: &mut Connection,
    event: &store::Event,
) -> Result<String, OpError> {
    let timezone = store::guild(conn, event.guild_id)?
        .map(|guild| guild.timezone)
        .unwrap_or_else(|| "UTC".into());
    let lang = i18n::lang_of(conn, event.guild_id);
    Ok(event_thread_name(lang, &event.title, event.starts_at, &timezone))
}

fn id_diff(desired: &[String], applied: &[String]) -> (Vec<String>, Vec<String>) {
    let want: HashSet<&str> = desired.iter().map(String::as_str).collect();
    let have: HashSet<&str> = applied.iter().map(String::as_str).collect();
    let mut add: Vec<String> = want.difference(&have).map(|id| (*id).to_string()).collect();
    let mut remove: Vec<String> = have.difference(&want).map(|id| (*id).to_string()).collect();
    add.sort();
    remove.sort();
    (add, remove)
}

fn role_color(color: &str) -> u32 {
    u32::from_str_radix(color.trim_start_matches('#'), 16).unwrap_or(0x1e3a6e)
}

/// Body of the ephemeral card shown before a Discord delete. The title of that card is
/// "Delete event". A posted event is cancelled and its post is removed. Anything else
/// that can still be deleted is removed outright.
pub fn delete_confirm_body(
    lang: Lang,
    title: &str,
    starts_at: DateTime<Utc>,
    url: Option<&str>,
    posted: bool,
) -> String {
    let fallback = tr!(lang, "post-event-fallback");
    let name = title.trim();
    let name = if name.is_empty() { fallback.as_str() } else { name };
    let label = trim_chars(&name.replace(['\n', '\r'], " "), 200);
    let label = label.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
    let head = match url.map(str::trim).filter(|url| !url.is_empty()) {
        Some(url) => format!("[{label}]({url})"),
        None => label,
    };
    let when = format!(" · <t:{}:R>", starts_at.timestamp());
    let consequence = if posted {
        tr!(lang, "post-delete-posted")
    } else {
        tr!(lang, "post-delete-draft")
    };
    format!("{head}{when}\n\n{consequence}")
}

/// Text that sits above the embed. Discord only notifies `@everyone` and roles from this
/// line, and only the first time each mention appears.
pub fn mention_line(everyone: bool, role_discord_ids: &[String]) -> String {
    let mut parts = Vec::new();
    if everyone {
        parts.push("@everyone".to_string());
    }
    for id in role_discord_ids {
        parts.push(format!("<@&{id}>"));
    }
    let mut line = String::new();
    for part in parts {
        let gap = if line.is_empty() { 0 } else { 1 };
        if line.chars().count() + gap + part.chars().count() > 2000 {
            break;
        }
        if gap == 1 {
            line.push(' ');
        }
        line.push_str(&part);
    }
    line
}

fn mention_discord_roles(conn: &mut Connection, event: &store::Event) -> Result<Vec<String>, OpError> {
    let ids = store::event_role_ids(conn, event, "mention")?;
    mention_role_ids(conn, &ids)
}

/// The Discord ids of roles a post may mention: not @everyone or @here, which
/// go by their own switch, and not roles Marian never learned the id of.
pub(super) fn mention_role_ids(conn: &mut Connection, ids: &[i64]) -> Result<Vec<String>, OpError> {
    let mut roles = Vec::new();
    for &role_id in ids {
        let Some(role) = store::role(conn, role_id)? else {
            continue;
        };
        if role.name.eq_ignore_ascii_case("@everyone") || role.name.eq_ignore_ascii_case("@here") {
            continue;
        }
        let Some(discord_id) = role.discord_id else {
            continue;
        };
        let discord_id = discord_id.trim();
        if discord_id.is_empty() || !discord_id.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        roles.push(discord_id.to_string());
    }
    roles.sort();
    roles.dedup();
    Ok(roles)
}

pub fn discord_view(
    conn: &mut Connection,
    event_id: i64,
    public_url: &str,
) -> Result<Option<DiscordView>, OpError> {
    let event = must_event(conn, event_id)?;
    if event.status != "posted" && event.discord_message_id.is_none() {
        return Ok(None);
    }
    let channel = must_channel(conn, event.channel_id)?;
    let Some(channel_id) = channel.discord_id else {
        return Ok(None);
    };
    let guild = must_guild(conn, event.guild_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    let lang = Lang::from_code(&settings.language).unwrap_or_default();
    let start = event.starts_at;
    let end = start + chrono::Duration::minutes(event.duration_minutes);
    let mut body = String::new();
    if !event.description.trim().is_empty() {
        body.push_str(event.description.trim());
        body.push_str("\n\n");
    }
    let location = event.location.trim();
    if !location.is_empty() {
        body.push_str(&format!("**{}**\n📍 {location}\n", tr!(lang, "post-location")));
    }
    body.push_str(&format!(
        "**{}**\n<t:{}:F> - <t:{}:t>\n",
        tr!(lang, "post-time"),
        start.timestamp(),
        end.timestamp()
    ));
    let plus = crate::ics::calendar_template(&event)?;
    let web = web_event_url(public_url, guild.discord_id.as_deref(), &event.public_id);
    match web.as_deref() {
        Some(url) => body.push_str(&format!(
            "[+]({plus}) · [{}]({url})\n",
            tr!(lang, "post-view-on-web"),
        )),
        None => body.push_str(&format!("[+]({plus})\n")),
    }
    body.push_str(&format!("🕐 <t:{}:R>", start.timestamp()));
    if event.status != "posted" {
        body.push_str(&format!("\n\n_{}_", status_text(lang, &event.status)));
    }
    let options = store::options_for(conn, &event)?;
    let signups = store::signups_for(conn, &event)?;
    let memberships = store::list_memberships(conn, event.guild_id)?;
    let names: HashMap<i64, String> = memberships.into_iter().map(|user| (user.id, user.name)).collect();
    let mut fields = Vec::new();
    let mut buttons = Vec::new();
    let closed = event.status != "posted" || event.signups_closed;
    // A finished event's post is a record; nothing on it can be pressed.
    let finished = matches!(event.status.as_str(), "archived" | "cancelled" | "skipped");
    for opt in &options {
        let confirmed: Vec<_> = signups
            .iter()
            .filter(|signup| signup.option_id == opt.id && signup.state == "confirmed")
            .collect();
        // Places: each person and the guests they bring.
        let waiting: i64 = signups
            .iter()
            .filter(|signup| signup.option_id == opt.id && signup.state == "waitlisted")
            .map(|signup| 1 + signup.guests)
            .sum();
        let taken: i64 = confirmed.iter().map(|signup| 1 + signup.guests).sum();
        let count = match opt.capacity {
            Some(cap) => format!("{taken}/{cap}"),
            None => taken.to_string(),
        };
        let mut lines = Vec::new();
        if settings.show_attendees {
            lines.extend(confirmed.iter().filter_map(|signup| {
                names.get(&signup.membership_id).map(|name| match signup.guests {
                    0 => name.clone(),
                    guests => format!("{name} (+{guests})"),
                })
            }));
        }
        if waiting > 0 {
            lines.push(tr!(lang, "post-waiting", count = waiting));
        }
        let value = if lines.is_empty() {
            "-".to_string()
        } else {
            lines.join("\n")
        };
        fields.push(DiscordField {
            name: trim_chars(&format!("{} **{}** ({count})", opt.emoji, opt.name), 256),
            value: trim_chars(&value, 1024),
            inline: true,
        });
        buttons.push(DiscordButton {
            role: ButtonRole::Signup(opt.public_id),
            // An option without an emoji still needs something on its button.
            label: if opt.emoji.trim().is_empty() {
                tr!(lang, "post-option-fallback")
            } else {
                String::new()
            },
            emoji: opt.emoji.clone(),
            tone: ButtonTone::Secondary,
            disabled: closed,
        });
        if opt.max_guests > 0 {
            buttons.push(DiscordButton {
                role: ButtonRole::Guests(opt.public_id),
                label: tr!(lang, "post-guests"),
                emoji: opt.emoji.clone(),
                tone: ButtonTone::Secondary,
                disabled: closed,
            });
        }
    }
    if finished {
        buttons.clear();
    }
    let mention_roles = mention_discord_roles(conn, &event)?;
    let creator = match event.created_by {
        Some(id) => store::membership(conn, id)?,
        None => None,
    }
    .map(|user| user.name)
    .filter(|name| !name.trim().is_empty())
    .unwrap_or_else(|| "Marian".into());
    let image_url = event.image_url.as_deref().and_then(|url| {
        let url = url.trim();
        if url.starts_with("https://") || url.starts_with("http://") {
            Some(url.to_string())
        } else {
            None
        }
    });
    Ok(Some(DiscordView {
        event: event.public_id,
        channel_id,
        message_id: event.discord_message_id.clone(),
        title: event.title,
        body,
        color: u32::from_str_radix(event.color.trim_start_matches('#'), 16).unwrap_or(0x1e3a6e),
        fields,
        footer: tr!(lang, "post-created-by", name = creator),
        image_url,
        buttons,
        mention_everyone: event.mention_everyone,
        mention_roles,
    }))
}

pub fn web_event_url(
    public_url: &str,
    server: Option<&str>,
    public_id: impl std::fmt::Display,
) -> Option<String> {
    let base = public_url.trim().trim_end_matches('/');
    let server = server.filter(|id| !id.is_empty())?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/servers/{server}/events/{public_id}"))
}

pub fn remember_discord_message(
    conn: &mut Connection,
    event_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
        .set(schema::events::discord_message_id.eq(message_id))
        .execute(conn)?;
    Ok(())
}

#[derive(Debug)]
pub struct ChannelReverse {
    pub name: String,
    pub messages: Vec<String>,
    pub repost: Vec<i64>,
}

pub fn channel_reverse_plan(
    conn: &mut Connection,
    guild_id: i64,
    discord_channel_id: &str,
) -> Result<ChannelReverse, OpError> {
    let Some(channel) = store::channel_by_discord(conn, guild_id, discord_channel_id)? else {
        return Err(OpError::from(crate::msg!("err-sort-where")));
    };
    if channel.kind != "event" {
        return Err(OpError::from(crate::msg!("err-sort-where")));
    }
    let mut posts = Vec::new();
    for event in store::list_events(conn, guild_id)? {
        if event.channel_id != channel.id {
            continue;
        }
        let Some(message_id) = event.discord_message_id.as_deref() else {
            continue;
        };
        let message_id = message_id.trim();
        if message_id.is_empty() {
            continue;
        }
        let Some(snowflake) = message_id.parse::<u64>().ok() else {
            continue;
        };
        posts.push((snowflake, event.id, message_id.to_string()));
    }
    posts.sort_by_key(|post| post.0);
    if posts.len() < 2 {
        return Err(OpError::from(crate::msg!("err-sort-nothing")));
    }
    let messages = posts.iter().map(|post| post.2.clone()).collect();
    let repost = posts.iter().rev().map(|post| post.1).collect();
    Ok(ChannelReverse {
        name: channel.name,
        messages,
        repost,
    })
}

pub fn detach_discord_posts(conn: &mut Connection, event_ids: &[i64]) -> Result<(), OpError> {
    for event_id in event_ids {
        diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
            .set(schema::events::discord_message_id.eq(None::<String>))
            .execute(conn)?;
        diesel::update(schema::threads::table.filter(schema::threads::event_id.eq(event_id)))
            .set(schema::threads::discord_id.eq(None::<String>))
            .execute(conn)?;
    }
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::guilds)]
struct NewGuild<'a> {
    name: &'a str,
    timezone: &'a str,
    discord_id: &'a str,
}

pub fn ensure_discord_guild(
    conn: &mut Connection,
    discord_id: &str,
    name: &str,
) -> Result<store::Guild, OpError> {
    if let Some(guild) = store::guild_by_discord(conn, discord_id)? {
        if guild.name != name && !name.is_empty() {
            diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(guild.id)))
                .set(schema::guilds::name.eq(name))
                .execute(conn)?;
        }
        return store::guild(conn, guild.id)?.ok_or(OpError::NotFound);
    }
    let name = if name.is_empty() {
        "Discord server"
    } else {
        name
    };
    let id = diesel::insert_into(schema::guilds::table)
        .values(&NewGuild {
            name,
            timezone: "UTC",
            discord_id,
        })
        .returning(schema::guilds::id)
        .get_result(conn)?;
    store::insert_default_settings(conn, id)?;
    store::guild(conn, id)?.ok_or(OpError::NotFound)
}

#[derive(Insertable)]
#[diesel(table_name = schema::memberships)]
struct NewMembership<'a> {
    user_id: i64,
    guild_id: i64,
    name: &'a str,
    handle: &'a str,
    color: &'a str,
    reminders_enabled: bool,
    server_manager: bool,
}

/// The member's membership, made if it's new, under `name`: the name they go
/// by in this server, which replaces the one stored. With it come the events
/// whose posts still show the old name; see [`follow_discord_name`].
pub fn ensure_discord_user(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<(Membership, Vec<i64>), OpError> {
    match follow_discord_name(conn, guild_id, discord_id, name)? {
        Some(found) => Ok(found),
        None => Ok((new_discord_user(conn, guild_id, discord_id, name)?, Vec::new())),
    }
}

/// The member's membership, made under `name` if it's new. For a name that
/// may not be the one they go by here, like the global name the web sign-in
/// gets: a stored name, perhaps a server nickname, is kept.
pub fn admit_discord_user(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<Membership, OpError> {
    match store::membership_by_discord(conn, guild_id, discord_id)? {
        Some(user) => Ok(user),
        None => new_discord_user(conn, guild_id, discord_id, name),
    }
}

/// Renames a member Marian already knows to the name they go by in this
/// server. Nobody new is added. A rename also gives the events whose posts
/// list the member under the old name and are still to be redrawn: see
/// [`posts_listing`].
pub fn follow_discord_name(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<Option<(Membership, Vec<i64>)>, OpError> {
    let Some(user) = store::membership_by_discord(conn, guild_id, discord_id)? else {
        return Ok(None);
    };
    if user.name == name || name.is_empty() {
        return Ok(Some((user, Vec::new())));
    }
    diesel::update(schema::memberships::table.filter(schema::memberships::id.eq(user.id)))
        .set(schema::memberships::name.eq(name))
        .execute(conn)?;
    let redraw = posts_listing(conn, user.id, Utc::now())?;
    let user = store::membership(conn, user.id)?.ok_or(OpError::NotFound)?;
    Ok(Some((user, redraw)))
}

/// Keeps a member's picture as Discord has it; `None` clears one they took
/// down. Posts don't show pictures, so nothing needs redrawing.
pub fn follow_discord_avatar(conn: &mut Connection, member: &Membership, avatar: Option<&str>) -> Result<(), OpError> {
    if member.avatar.as_deref() == avatar {
        return Ok(());
    }
    diesel::update(schema::memberships::table.filter(schema::memberships::id.eq(member.id)))
        .set(schema::memberships::avatar.eq(avatar))
        .execute(conn)?;
    Ok(())
}

/// Events whose Discord post lists the member among the people signed up and
/// hasn't ended. A post for an event that's over keeps the names it was last
/// drawn with, even while it still takes signups.
fn posts_listing(conn: &mut Connection, membership_id: i64, now: DateTime<Utc>) -> Result<Vec<i64>, OpError> {
    let signed_up = schema::signups::table
        .filter(schema::signups::membership_id.eq(membership_id))
        .filter(schema::signups::state.eq("confirmed"))
        .select(schema::signups::event_id);
    let events: Vec<(i64, DateTime<Utc>, i64)> = schema::events::table
        .filter(schema::events::id.eq_any(signed_up))
        .filter(schema::events::status.eq("posted"))
        .filter(schema::events::discord_message_id.is_not_null())
        .select((schema::events::id, schema::events::starts_at, schema::events::duration_minutes))
        .load(conn)?;
    Ok(events
        .into_iter()
        .filter(|(_, starts_at, minutes)| *starts_at + chrono::Duration::minutes(*minutes) > now)
        .map(|(id, _, _)| id)
        .collect())
}

fn new_discord_user(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<Membership, OpError> {
    let user_id = store::ensure_user(conn, discord_id)?;
    let handle = unique_handle(conn, guild_id, name, discord_id)?;
    let color = [
        "#1e3a6e", "#2f6b4f", "#8a6232", "#6b3a6b", "#8d3a32", "#243038",
    ][discord_id
        .bytes()
        .fold(0usize, |acc, byte| acc.wrapping_add(byte as usize))
        % 6];
    let display = if name.trim().is_empty() {
        handle.as_str()
    } else {
        name.trim()
    };
    let id = diesel::insert_into(schema::memberships::table)
        .values(&NewMembership {
            user_id,
            guild_id,
            name: display,
            handle: &handle,
            color,
            reminders_enabled: true,
            server_manager: false,
        })
        .returning(schema::memberships::id)
        .get_result(conn)?;
    store::membership(conn, id)?.ok_or(OpError::NotFound)
}

pub fn set_server_manager(conn: &mut Connection, membership_id: i64, server_manager: bool) -> Result<(), OpError> {
    diesel::update(schema::memberships::table.filter(schema::memberships::id.eq(membership_id)))
        .set(schema::memberships::server_manager.eq(server_manager))
        .execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::channels)]
struct NewChannel<'a> {
    guild_id: i64,
    name: &'a str,
    kind: &'a str,
    position: i64,
    discord_id: &'a str,
}

pub fn ensure_discord_channel(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<Channel, OpError> {
    if let Some(channel) = store::channel_by_discord(conn, guild_id, discord_id)? {
        return Ok(channel);
    }
    let slug = free_channel_slug(conn, guild_id, discord_id, name, None)?;
    let position = schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .select(diesel::dsl::max(schema::channels::position))
        .first::<Option<i64>>(conn)?
        .unwrap_or(0)
        + 1;
    let id = diesel::insert_into(schema::channels::table)
        .values(&NewChannel {
            guild_id,
            name: &slug,
            kind: "text",
            position,
            discord_id,
        })
        .returning(schema::channels::id)
        .get_result(conn)?;
    store::channel(conn, id)?.ok_or(OpError::NotFound)
}

/// The slug a Discord channel goes by here, with a bit of its id added when
/// another channel (other than `except`) already has the name.
fn free_channel_slug(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
    except: Option<i64>,
) -> Result<String, OpError> {
    let raw = slug_channel(name);
    let slug = if raw.is_empty() {
        format!(
            "channel-{}",
            &discord_id[discord_id.len().saturating_sub(6)..]
        )
    } else {
        raw
    };
    let taken = store::channel_by_name(conn, guild_id, &slug)?
        .is_some_and(|other| Some(other.id) != except);
    Ok(if taken {
        format!(
            "{slug}-{}",
            &discord_id[discord_id.len().saturating_sub(4)..]
        )
    } else {
        slug
    })
}

/// Brings the stored channel for a Discord text channel up to date: adds it,
/// follows a rename, or brings it back. An event channel Marian is still
/// creating under this name is taken over, so it isn't stored twice.
pub fn sync_discord_channel(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<Channel, OpError> {
    let Some(channel) = store::channel_by_discord(conn, guild_id, discord_id)? else {
        let pending = store::channel_by_name(conn, guild_id, &slug_channel(name))?.filter(|channel| {
            channel.kind == "event" && channel.discord_id.as_deref().is_none_or(str::is_empty)
        });
        let Some(pending) = pending else {
            return ensure_discord_channel(conn, guild_id, discord_id, name);
        };
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(pending.id)))
            .set(schema::channels::discord_id.eq(discord_id))
            .execute(conn)?;
        return store::channel(conn, pending.id)?.ok_or(OpError::NotFound);
    };
    let renamed = !slug_channel(name).is_empty() && slug_channel(name) != channel.name;
    if !renamed && channel.gone_at.is_none() {
        return Ok(channel);
    }
    let slug = if renamed {
        free_channel_slug(conn, guild_id, discord_id, name, Some(channel.id))?
    } else {
        channel.name.clone()
    };
    diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel.id)))
        .set((
            schema::channels::name.eq(&slug),
            schema::channels::gone_at.eq(None::<DateTime<Utc>>),
        ))
        .execute(conn)?;
    store::channel(conn, channel.id)?.ok_or(OpError::NotFound)
}

/// Marks the channel for a deleted Discord channel as gone.
pub fn discord_channel_gone(conn: &mut Connection, guild_id: i64, discord_id: &str) -> Result<(), OpError> {
    diesel::update(
        schema::channels::table
            .filter(schema::channels::guild_id.eq(guild_id))
            .filter(schema::channels::discord_id.eq(discord_id))
            .filter(schema::channels::gone_at.is_null()),
    )
    .set(schema::channels::gone_at.eq(Utc::now()))
    .execute(conn)?;
    Ok(())
}

/// Matches the server's channels to the text channels Discord lists, as
/// (Discord id, name): new ones are added, renames followed, missing ones gone.
pub fn sync_discord_channels(
    conn: &mut Connection,
    guild_id: i64,
    present: &[(String, String)],
) -> Result<(), OpError> {
    for (discord_id, name) in present {
        sync_discord_channel(conn, guild_id, discord_id, name)?;
    }
    let listed: HashSet<&str> = present.iter().map(|(id, _)| id.as_str()).collect();
    for channel in store::list_channels(conn, guild_id)? {
        let Some(discord_id) = channel.discord_id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        if !listed.contains(discord_id) {
            discord_channel_gone(conn, guild_id, discord_id)?;
        }
    }
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::roles)]
struct NewDiscordRole<'a> {
    guild_id: i64,
    name: &'a str,
    color: &'a str,
    discord_id: &'a str,
}

/// A Discord role colour as stored here; Discord's "no colour" reads as grey.
fn stored_role_color(colour: &str) -> String {
    let hex = colour.trim_start_matches('#');
    if hex.is_empty() || hex.trim_start_matches('0').is_empty() {
        "#99aab5".to_string()
    } else {
        format!("#{hex}")
    }
}

/// Brings the stored role for a Discord role up to date, matched by its
/// Discord id: adds it, follows a rename or colour change, or brings it back.
/// A role Marian is still creating under this name is taken over, so it isn't
/// stored twice.
pub fn sync_discord_role(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
    colour: &str,
) -> Result<Role, OpError> {
    let color = stored_role_color(colour);
    let stored = match store::role_by_discord(conn, guild_id, discord_id)? {
        Some(role) => Some(role),
        None => store::role_by_name(conn, guild_id, name)?
            .filter(|role| role.discord_id.as_deref().is_none_or(str::is_empty)),
    };
    let Some(role) = stored else {
        let id = diesel::insert_into(schema::roles::table)
            .values(&NewDiscordRole {
                guild_id,
                name,
                color: &color,
                discord_id,
            })
            .returning(schema::roles::id)
            .get_result(conn)?;
        return store::role(conn, id)?.ok_or(OpError::NotFound);
    };
    let current = role.name == name
        && role.color == color
        && role.discord_id.as_deref() == Some(discord_id)
        && role.gone_at.is_none();
    if current {
        return Ok(role);
    }
    diesel::update(schema::roles::table.filter(schema::roles::id.eq(role.id)))
        .set((
            schema::roles::name.eq(name),
            schema::roles::color.eq(&color),
            schema::roles::discord_id.eq(discord_id),
            schema::roles::gone_at.eq(None::<DateTime<Utc>>),
        ))
        .execute(conn)?;
    store::role(conn, role.id)?.ok_or(OpError::NotFound)
}

/// Marks the role for a deleted Discord role as gone; nobody holds it any more.
pub fn discord_role_gone(conn: &mut Connection, guild_id: i64, discord_id: &str) -> Result<(), OpError> {
    let Some(role) = store::role_by_discord(conn, guild_id, discord_id)? else {
        return Ok(());
    };
    diesel::delete(schema::membership_roles::table.filter(schema::membership_roles::role_id.eq(role.id)))
        .execute(conn)?;
    if role.gone_at.is_none() {
        diesel::update(schema::roles::table.filter(schema::roles::id.eq(role.id)))
            .set(schema::roles::gone_at.eq(Utc::now()))
            .execute(conn)?;
    }
    Ok(())
}

/// Matches the server's roles to the roles Discord lists, as (Discord id,
/// name, colour): new ones are added, changes followed, missing ones gone.
pub fn sync_discord_roles(
    conn: &mut Connection,
    guild_id: i64,
    present: &[(String, String, String)],
) -> Result<(), OpError> {
    for (discord_id, name, colour) in present {
        sync_discord_role(conn, guild_id, discord_id, name, colour)?;
    }
    let listed: HashSet<&str> = present.iter().map(|(id, _, _)| id.as_str()).collect();
    for role in store::list_roles(conn, guild_id)? {
        let Some(discord_id) = role.discord_id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        if !listed.contains(discord_id) {
            discord_role_gone(conn, guild_id, discord_id)?;
        }
    }
    Ok(())
}

/// Stores a role's server-wide permissions; @everyone's id is the server's.
pub fn sync_discord_permissions(
    conn: &mut Connection,
    guild_id: i64,
    guild_discord_id: &str,
    role_discord_id: &str,
    permissions: u64,
) -> Result<(), OpError> {
    if role_discord_id == guild_discord_id {
        diesel::update(schema::guilds::table.find(guild_id))
            .set(schema::guilds::everyone_permissions.eq(permissions as i64))
            .execute(conn)?;
    } else {
        diesel::update(
            schema::roles::table
                .filter(schema::roles::guild_id.eq(guild_id))
                .filter(schema::roles::discord_id.eq(role_discord_id)),
        )
        .set(schema::roles::permissions.eq(permissions as i64))
        .execute(conn)?;
    }
    Ok(())
}

/// Replaces a Discord channel's stored permission overwrites with `overwrites`.
pub fn sync_channel_overwrites(
    conn: &mut Connection,
    guild_id: i64,
    channel_discord_id: &str,
    overwrites: &[super::access::Overwrite],
) -> Result<(), OpError> {
    let Some(channel) = store::channel_by_discord(conn, guild_id, channel_discord_id)? else {
        return Ok(());
    };
    diesel::delete(schema::channel_overwrites::table.filter(schema::channel_overwrites::channel_id.eq(channel.id)))
        .execute(conn)?;
    let rows: Vec<_> = overwrites
        .iter()
        .map(|o| {
            (
                schema::channel_overwrites::channel_id.eq(channel.id),
                schema::channel_overwrites::kind.eq(if o.member { "member" } else { "role" }),
                schema::channel_overwrites::target.eq(&o.target),
                schema::channel_overwrites::allow.eq(o.allow as i64),
                schema::channel_overwrites::deny.eq(o.deny as i64),
            )
        })
        .collect();
    diesel::insert_into(schema::channel_overwrites::table)
        .values(&rows)
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

fn unique_handle(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
    discord_id: &str,
) -> Result<String, OpError> {
    let mut base: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    base.make_ascii_lowercase();
    if base.is_empty() {
        base = format!("user{}", &discord_id[discord_id.len().saturating_sub(6)..]);
    }
    base = base.chars().take(24).collect();
    if store::membership_by_handle(conn, guild_id, &base)?.is_none() {
        return Ok(base);
    }
    let suffix = &discord_id[discord_id.len().saturating_sub(4)..];
    Ok(format!("{base}{suffix}"))
}

/// What to remove from an event channel after an event is posted or deleted.
/// Pinned messages and the channel's event posts stay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChannelPurge {
    pub channel_id: i64,
    pub discord_channel_id: String,
    pub keep: HashSet<String>,
}

pub fn event_channel_purge(
    conn: &mut Connection,
    event_id: i64,
) -> Result<Option<EventChannelPurge>, OpError> {
    let event = must_event(conn, event_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    if !settings.purge_channels {
        return Ok(None);
    }
    let channel = must_channel(conn, event.channel_id)?;
    if channel.kind != "event" {
        return Ok(None);
    }
    let Some(discord_channel_id) = channel.discord_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    let ids: Vec<Option<String>> = schema::events::table
        .filter(schema::events::channel_id.eq(channel.id))
        .select(schema::events::discord_message_id)
        .load(conn)?;
    // A routine's results reply sits under its event's post and stays with it.
    let results: Vec<Option<String>> = schema::routine_runs::table
        .inner_join(schema::events::table.on(schema::routine_runs::event_id.eq(schema::events::id.nullable())))
        .filter(schema::events::channel_id.eq(channel.id))
        .select(schema::routine_runs::results_message_id)
        .load(conn)?;
    let keep = ids.into_iter().chain(results).flatten().filter(|id| !id.is_empty()).collect();
    Ok(Some(EventChannelPurge {
        channel_id: channel.id,
        discord_channel_id,
        keep,
    }))
}

pub fn mark_channel_purged(conn: &mut Connection, channel_id: i64) -> Result<(), OpError> {
    diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel_id)))
        .set(schema::channels::purged_at.eq(Utc::now()))
        .execute(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection;

    use super::*;

    fn one_channel() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id) VALUES (41, 1, 'raids', 'text', 1, '100');
            ",
        )
        .unwrap();
        conn
    }

    fn names(conn: &mut Connection) -> Vec<String> {
        store::list_channels(conn, 1)
            .unwrap()
            .into_iter()
            .map(|channel| channel.name)
            .collect()
    }

    fn one_role() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES (51, 1, 'Raider', '#112233', '700');
            INSERT INTO users (id, discord_id) VALUES (61, 'u1');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            VALUES (71, 61, 1, 'Nell', 'nell', '#112233', true, false);
            INSERT INTO membership_roles (membership_id, role_id) VALUES (71, 51);
            ",
        )
        .unwrap();
        conn
    }

    fn role_names(conn: &mut Connection) -> Vec<String> {
        store::list_roles(conn, 1)
            .unwrap()
            .into_iter()
            .map(|role| role.name)
            .collect()
    }

    #[test]
    fn a_renamed_role_keeps_its_row() {
        let mut conn = one_role();
        let role = sync_discord_role(&mut conn, 1, "700", "Veteran", "aa0000").unwrap();
        assert_eq!((role.id, role.name.as_str(), role.color.as_str()), (51, "Veteran", "#aa0000"));
        assert_eq!(role_names(&mut conn), vec!["Veteran"]);
        assert!(store::membership_has_role(&mut conn, 71, 51).unwrap());
    }

    #[test]
    fn a_role_without_colour_reads_as_grey() {
        let mut conn = one_role();
        let role = sync_discord_role(&mut conn, 1, "701", "Plain", "000000").unwrap();
        assert_eq!(role.color, "#99aab5");
    }

    #[test]
    fn a_deleted_role_leaves_the_list_and_its_holders() {
        let mut conn = one_role();
        discord_role_gone(&mut conn, 1, "700").unwrap();
        assert!(role_names(&mut conn).is_empty());
        assert!(!store::membership_has_role(&mut conn, 71, 51).unwrap());
        assert!(store::role(&mut conn, 51).unwrap().unwrap().gone_at.is_some());

        let back = sync_discord_role(&mut conn, 1, "700", "Raider", "112233").unwrap();
        assert_eq!(back.id, 51);
        assert!(back.gone_at.is_none());
    }

    #[test]
    fn syncing_roles_drops_missing_ones_and_takes_over_unlinked_ones() {
        let mut conn = one_role();
        conn.batch_execute(
            "INSERT INTO roles (id, guild_id, name, color) VALUES (52, 1, 'Race attendee', '#1e3a6e');",
        )
        .unwrap();
        sync_discord_roles(&mut conn, 1, &[("800".into(), "Race attendee".into(), "1e3a6e".into())]).unwrap();
        let roles = store::list_roles(&mut conn, 1).unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!((roles[0].id, roles[0].discord_id.as_deref()), (52, Some("800")));
    }

    #[test]
    fn a_renamed_discord_channel_is_renamed_here() {
        let mut conn = one_channel();
        let channel = sync_discord_channel(&mut conn, 1, "100", "Raid Night").unwrap();
        assert_eq!((channel.id, channel.name.as_str()), (41, "raid-night"));

        sync_discord_channel(&mut conn, 1, "200", "general").unwrap();
        let clash = sync_discord_channel(&mut conn, 1, "200", "raid night").unwrap();
        assert_eq!(clash.name, "raid-night-200");
    }

    #[test]
    fn a_channel_keeps_letters_and_emoji_outside_ascii() {
        let mut conn = one_channel();
        let channel = sync_discord_channel(&mut conn, 1, "100", "wybierz-rolę").unwrap();
        assert_eq!((channel.id, channel.name.as_str()), (41, "wybierz-rolę"));

        let emoji = sync_discord_channel(&mut conn, 1, "200", "📢┃Ogłoszenia").unwrap();
        assert_eq!(emoji.name, "📢┃ogłoszenia");
        assert_eq!(slug_channel("#Gry i zabawy!"), "gry-i-zabawy");
    }

    #[test]
    fn a_deleted_channel_leaves_the_list_and_frees_its_name() {
        let mut conn = one_channel();
        discord_channel_gone(&mut conn, 1, "100").unwrap();
        assert!(names(&mut conn).is_empty());
        assert!(store::channel(&mut conn, 41).unwrap().unwrap().gone_at.is_some());

        let fresh = sync_discord_channel(&mut conn, 1, "300", "raids").unwrap();
        assert_ne!(fresh.id, 41);
        assert_eq!(fresh.name, "raids");
    }

    #[test]
    fn syncing_adds_new_channels_and_drops_missing_ones() {
        let mut conn = one_channel();
        conn.batch_execute(
            "INSERT INTO channels (id, guild_id, name, kind, position) VALUES (42, 1, 'pending', 'event', 2);",
        )
        .unwrap();
        sync_discord_channels(&mut conn, 1, &[("200".into(), "general".into())]).unwrap();
        assert_eq!(names(&mut conn), vec!["pending", "general"]);

        sync_discord_channels(&mut conn, 1, &[("100".into(), "raids".into())]).unwrap();
        assert_eq!(names(&mut conn), vec!["raids", "pending"]);
    }

    #[test]
    fn a_channel_marian_is_creating_is_taken_over_not_doubled() {
        let mut conn = one_channel();
        conn.batch_execute(
            "INSERT INTO channels (id, guild_id, name, kind, position) VALUES (42, 1, 'race-night', 'event', 2);",
        )
        .unwrap();
        let channel = sync_discord_channel(&mut conn, 1, "500", "race-night").unwrap();
        assert_eq!((channel.id, channel.kind.as_str()), (42, "event"));
        assert_eq!(channel.discord_id.as_deref(), Some("500"));
        assert_eq!(names(&mut conn), vec!["raids", "race-night"]);
    }

    #[test]
    fn only_a_posted_event_waits_for_its_post() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, discord_id) VALUES (1, 1, 'events', 'text', '100');
            INSERT INTO events (
              id, public_id, guild_id, channel_id, title, starts_at, duration_minutes,
              status, created_at, updated_at, discord_message_id
            ) VALUES
              (1, '01900000-0000-7000-8000-000000000041', 1, 1, 'Draft', '2026-10-14T18:00:00Z', 60, 'draft', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', NULL),
              (2, '01900000-0000-7000-8000-000000000042', 1, 1, 'Later', '2026-10-14T18:00:00Z', 60, 'scheduled', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', NULL),
              (3, '01900000-0000-7000-8000-000000000043', 1, 1, 'Going out', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', NULL),
              (4, '01900000-0000-7000-8000-000000000044', 1, 1, 'Out', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '55');
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        assert!(matches!(attendance(&mut conn, 1).unwrap(), Attendance::Idle), "a draft has nothing to give");
        assert!(matches!(attendance(&mut conn, 2).unwrap(), Attendance::Idle), "nor does a scheduled post");
        assert!(matches!(attendance(&mut conn, 3).unwrap(), Attendance::Waiting), "a posted event waits for its message");
        assert!(matches!(attendance(&mut conn, 4).unwrap(), Attendance::Ready(_)));
    }

    #[test]
    fn a_rename_redraws_only_posts_of_events_not_over() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, discord_id) VALUES (1, 1, 'social', '100');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell';
            INSERT INTO events (
              id, public_id, guild_id, channel_id, title, starts_at, duration_minutes,
              status, created_at, updated_at, discord_message_id
            ) VALUES
              (1, '01900000-0000-7000-8000-000000000051', 1, 1, 'Next week', '2026-10-21T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '51'),
              (2, '01900000-0000-7000-8000-000000000052', 1, 1, 'Under way', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '52'),
              (3, '01900000-0000-7000-8000-000000000053', 1, 1, 'Over, still open', '2026-10-13T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '53'),
              (4, '01900000-0000-7000-8000-000000000054', 1, 1, 'Waiting', '2026-10-21T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '54'),
              (5, '01900000-0000-7000-8000-000000000055', 1, 1, 'Not out yet', '2026-10-21T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', NULL),
              (6, '01900000-0000-7000-8000-000000000056', 1, 1, 'Archived', '2026-10-21T18:00:00Z', 60, 'archived', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '56');
            INSERT INTO option_library (guild_id, name, emoji, attending) VALUES
              (1, 'Accepted', '✅', true) ON CONFLICT DO NOTHING;
            INSERT INTO signup_options (id, event_id, type_id, position)
            SELECT event, event, (SELECT id FROM option_library WHERE guild_id = 1 AND name = 'Accepted'), 0
            FROM (VALUES (1), (2), (3), (4), (5), (6)) AS ids(event);
            INSERT INTO signups (event_id, option_id, membership_id, state, created_at) VALUES
              (1, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (2, 2, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (3, 3, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (4, 4, 1, 'waitlisted', '2026-10-13T00:00:00Z'),
              (5, 5, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (6, 6, 1, 'confirmed', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        let now = clock::parse_stamp("2026-10-14T18:30:00Z").unwrap();
        let mut redraw = posts_listing(&mut conn, 1, now).unwrap();
        redraw.sort();
        assert_eq!(redraw, vec![1, 2], "a post that's over keeps its names, even while it takes signups");
    }

    #[test]
    fn the_channel_card_lists_names_in_columns() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, discord_id) VALUES (1, 1, 'social', '100');
            INSERT INTO users (discord_id) VALUES ('nell'), ('june');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#112233' FROM users WHERE discord_id = 'june';
            INSERT INTO events (
              public_id, guild_id, channel_id, title, description, starts_at, duration_minutes,
              status, created_by, created_at, updated_at
            ) VALUES (
              '01900000-0000-7000-8000-000000000010', 1, 1, 'Shore cleanup', 'Bring gloves',
              '2026-10-14T18:00:00Z', 60, 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'
            );
            INSERT INTO option_library (guild_id, name, emoji, attending) VALUES
              (1, 'Accepted', '✅', true), (1, 'Declined', '❌', false) ON CONFLICT DO NOTHING;
            INSERT INTO signup_options (id, event_id, type_id, position) VALUES
              (1, 1, (SELECT id FROM option_library WHERE guild_id = 1 AND name = 'Accepted'), 0),
              (2, 1, (SELECT id FROM option_library WHERE guild_id = 1 AND name = 'Declined'), 1);
            INSERT INTO signups (event_id, option_id, membership_id, state, created_at) VALUES
              (1, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (1, 1, 2, 'confirmed', '2026-10-13T00:00:00Z'),
              (1, 2, 2, 'waitlisted', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let view = discord_view(&mut conn, 1, "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert!(
            view.body.starts_with("Bring gloves\n\n**Time**"),
            "{}",
            view.body
        );
        assert!(
            view.body.contains(
                "[View on web](https://marian.example.com/servers/4242/events/01900000-0000-7000-8000-000000000010)"
            ),
            "{}",
            view.body
        );
        assert!(view.body.contains("🕐 <t:"), "{}", view.body);
        assert!(!view.body.contains("**Location**"), "{}", view.body);
        conn.batch_execute("UPDATE events SET location = 'The dock' WHERE id = 1")
            .unwrap();
        let placed = discord_view(&mut conn, 1, "https://marian.example.com")
            .unwrap()
            .unwrap();
        let location_at = placed.body.find("**Location**").unwrap();
        let time_at = placed.body.find("**Time**").unwrap();
        assert!(location_at < time_at, "{}", placed.body);
        assert!(placed.body.contains("📍 The dock"), "{}", placed.body);
        assert_eq!(view.footer, "Created by Nell");
        assert_eq!(view.fields.len(), 2);
        assert!(view.fields.iter().all(|field| field.inline));
        assert_eq!(view.fields[0].name, "✅ **Accepted** (2)");
        assert_eq!(view.fields[0].value, "Nell\nJune");
        assert_eq!(view.fields[1].name, "❌ **Declined** (0)");
        assert_eq!(view.fields[1].value, "1 waiting");
        assert_eq!(view.buttons.len(), 2);
        assert!(!view.mention_everyone);
        assert!(view.mention_roles.is_empty());
    }

    #[test]
    fn the_post_mentions_everyone_and_the_checked_roles() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, discord_id) VALUES (1, 1, 'social', '100');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell';
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES
              (1, 1, 'Raiders', '#112233', '111'),
              (2, 1, 'Local only', '#112233', NULL),
              (3, 1, 'Officers', '#112233', '333'),
              (4, 1, '@everyone', '#112233', '4242');
            INSERT INTO events (
              public_id, guild_id, channel_id, title, starts_at, status, mention_everyone,
              created_by, created_at, updated_at
            ) VALUES (
              '01900000-0000-7000-8000-000000000010', 1, 1, 'Shore cleanup',
              '2026-10-14T18:00:00Z', 'posted', true, 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'
            );
            INSERT INTO event_roles (event_id, role_id, kind) VALUES
              (1, 3, 'mention'),
              (1, 1, 'mention'),
              (1, 2, 'mention'),
              (1, 4, 'mention'),
              (1, 3, 'restrict');
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let view = discord_view(&mut conn, 1, "https://marian.example.com")
            .unwrap()
            .unwrap();
        assert!(view.mention_everyone);
        assert_eq!(view.mention_roles, vec!["111".to_string(), "333".to_string()]);
        assert_eq!(
            mention_line(view.mention_everyone, &view.mention_roles),
            "@everyone <@&111> <@&333>"
        );
    }

    #[test]
    fn the_post_counts_guests_and_offers_a_guests_button() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, discord_id) VALUES (1, 1, 'social', '100');
            INSERT INTO users (discord_id) VALUES ('nell'), ('june');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#223344' FROM users WHERE discord_id = 'june';
            INSERT INTO option_library (id, guild_id, name, emoji, attending) VALUES
              (101, 1, 'Yes', '✅', true),
              (102, 1, 'No', '❌', false);
            INSERT INTO events (
              public_id, guild_id, channel_id, title, starts_at, status,
              created_by, created_at, updated_at
            ) VALUES (
              '01900000-0000-7000-8000-000000000010', 1, 1, 'Dinner',
              '2026-10-14T18:00:00Z', 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'
            );
            INSERT INTO signup_options (id, event_id, type_id, position, capacity, max_guests) VALUES
              (1, 1, 101, 0, 10, 3),
              (2, 1, 102, 1, NULL, 0);
            INSERT INTO signups (event_id, option_id, membership_id, state, created_at, guests) VALUES
              (1, 1, 1, 'confirmed', '2026-10-13T00:00:00Z', 2),
              (1, 1, 2, 'confirmed', '2026-10-13T00:00:00Z', 0);
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let view = discord_view(&mut conn, 1, "https://marian.example.com").unwrap().unwrap();
        assert_eq!(view.fields[0].name, "✅ **Yes** (4/10)");
        assert_eq!(view.fields[0].value, "Nell (+2)\nJune");
        let roles: Vec<&ButtonRole> = view.buttons.iter().map(|button| &button.role).collect();
        assert!(matches!(
            roles.as_slice(),
            [ButtonRole::Signup(_), ButtonRole::Guests(_), ButtonRole::Signup(_)]
        ));
        assert_eq!(view.buttons[1].emoji, "✅");
        assert_eq!(view.buttons[1].label, "+X");
    }

    #[test]
    fn delete_confirm_names_the_event_and_says_what_delete_does() {
        let when = clock::parse_stamp("2026-10-16T14:00:00Z").unwrap();
        let posted = delete_confirm_body(
            Lang::En,
            "Test",
            when,
            Some("https://marian.example.com/servers/1/events/abc"),
            true,
        );
        assert!(
            posted.starts_with(
                "[Test](https://marian.example.com/servers/1/events/abc) · <t:"
            ),
            "{posted}"
        );
        assert!(posted.contains(":R>\n\nThis removes the Discord post. The event is marked cancelled."));
        let draft = delete_confirm_body(Lang::En, "Shore", when, None, false);
        assert!(draft.starts_with("Shore · <t:"), "{draft}");
        assert!(draft.contains("This deletes the event."));
    }

    #[test]
    fn signing_in_does_not_make_the_first_member_a_server_manager() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');",
        )
        .unwrap();
        let (first, _) = ensure_discord_user(&mut conn, 1, "100", "Nell").unwrap();
        assert!(!first.server_manager);
        let (second, _) = ensure_discord_user(&mut conn, 1, "200", "June").unwrap();
        assert!(!second.server_manager);
        set_server_manager(&mut conn, first.id, true).unwrap();
        assert!(store::membership(&mut conn, first.id).unwrap().unwrap().server_manager);
        set_server_manager(&mut conn, first.id, false).unwrap();
        assert!(!store::membership(&mut conn, first.id).unwrap().unwrap().server_manager);
    }

    #[test]
    fn a_server_nickname_stays_through_a_web_sign_in() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');",
        )
        .unwrap();
        assert!(follow_discord_name(&mut conn, 1, "100", "Nellie").unwrap().is_none(), "nobody new is added");
        let signed_in = admit_discord_user(&mut conn, 1, "100", "Nell").unwrap();
        assert_eq!(signed_in.name, "Nell", "a new member starts with the name they signed in with");

        let (clicked, _) = ensure_discord_user(&mut conn, 1, "100", "Nellie").unwrap();
        assert_eq!((clicked.id, clicked.name.as_str()), (signed_in.id, "Nellie"));
        let again = admit_discord_user(&mut conn, 1, "100", "Nell").unwrap();
        assert_eq!(again.name, "Nellie", "the global name doesn't replace the nickname");

        let (renamed, _) = follow_discord_name(&mut conn, 1, "100", "Nells").unwrap().unwrap();
        assert_eq!(renamed.name, "Nells");
    }

    #[test]
    fn a_member_picture_follows_discord_and_can_be_taken_down() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');",
        )
        .unwrap();
        let (member, _) = ensure_discord_user(&mut conn, 1, "100", "Nell").unwrap();
        assert_eq!(member.avatar, None, "a new member shows their initial");

        let url = "https://cdn.discordapp.com/avatars/100/abc.webp?size=64";
        follow_discord_avatar(&mut conn, &member, Some(url)).unwrap();
        let member = store::membership(&mut conn, member.id).unwrap().unwrap();
        assert_eq!(member.avatar.as_deref(), Some(url));

        let (renamed, _) = ensure_discord_user(&mut conn, 1, "100", "Nellie").unwrap();
        assert_eq!(renamed.avatar.as_deref(), Some(url), "a rename keeps the picture");

        follow_discord_avatar(&mut conn, &renamed, None).unwrap();
        assert_eq!(store::membership(&mut conn, member.id).unwrap().unwrap().avatar, None);
    }

    #[test]
    fn mention_line_stops_before_a_mention_would_pass_the_limit() {
        let huge = "1".repeat(1990);
        assert_eq!(mention_line(true, &[huge]), "@everyone");
        assert_eq!(mention_line(false, &["42".into()]), "<@&42>");
    }

    #[test]
    fn sort_reverses_posts_in_an_event_channel() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, discord_id) VALUES
              (1, 1, 'events', 'event', '100'),
              (2, 1, 'general', 'text', '200');
            INSERT INTO events (
              id, public_id, guild_id, channel_id, title, starts_at, duration_minutes,
              status, created_at, updated_at, discord_message_id
            ) VALUES
              (1, '01900000-0000-7000-8000-000000000011', 1, 1, 'First', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '10'),
              (2, '01900000-0000-7000-8000-000000000012', 1, 1, 'Second', '2026-10-15T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '20'),
              (3, '01900000-0000-7000-8000-000000000013', 1, 1, 'Third', '2026-10-16T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '30'),
              (4, '01900000-0000-7000-8000-000000000014', 1, 2, 'Elsewhere', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '40');
            INSERT INTO threads (event_id, name, discord_id) VALUES (1, 'First', '900');
            ",
        )
        .unwrap();
        let refused = channel_reverse_plan(&mut conn, 1, "200").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Sort reverses the posts in an event channel."
        );
        let plan = channel_reverse_plan(&mut conn, 1, "100").unwrap();
        assert_eq!(plan.name, "events");
        assert_eq!(plan.messages, vec!["10", "20", "30"]);
        assert_eq!(plan.repost, vec![3, 2, 1]);
        detach_discord_posts(&mut conn, &plan.repost).unwrap();
        let first = store::event(&mut conn, 1).unwrap().unwrap();
        assert!(first.discord_message_id.is_none());
        assert!(store::thread_discord_id(&mut conn, 1).unwrap().is_none());
        let elsewhere = store::event(&mut conn, 4).unwrap().unwrap();
        assert_eq!(elsewhere.discord_message_id.as_deref(), Some("40"));
    }

    #[test]
    fn purge_keeps_event_posts_in_an_event_channel() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, kind, discord_id) VALUES
              (1, 1, 'events', 'event', '100'),
              (2, 1, 'general', 'text', '200'),
              (3, 1, 'unlinked', 'event', NULL);
            INSERT INTO events (
              id, public_id, guild_id, channel_id, title, starts_at, duration_minutes,
              status, created_at, updated_at, discord_message_id
            ) VALUES
              (1, '01900000-0000-7000-8000-000000000021', 1, 1, 'First', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '10'),
              (2, '01900000-0000-7000-8000-000000000022', 1, 1, 'Second', '2026-10-15T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '20'),
              (3, '01900000-0000-7000-8000-000000000023', 1, 2, 'Elsewhere', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', '40'),
              (4, '01900000-0000-7000-8000-000000000024', 1, 3, 'Local', '2026-10-14T18:00:00Z', 60, 'posted', '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z', NULL);
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        // The second event came from a routine poll; its results reply stays too.
        conn.batch_execute(
            "
            INSERT INTO event_templates (id, guild_id, channel_id, title, duration_minutes, created_at)
              VALUES (1, 1, 1, 'Game night', 60, '2026-10-01T00:00:00Z');
            INSERT INTO routines (id, public_id, guild_id, template_id, prompt, start_minutes, poll_weekday,
              poll_minutes, lock_weekday, lock_minutes, days_json, created_at)
              VALUES (1, '01900000-0000-7000-8000-000000000031', 1, 1, '', 1140, 6, 600, 1, 1080, '{3}', '2026-10-01T00:00:00Z');
            INSERT INTO routine_runs (routine_id, opens_at, locks_at, status, days_json, event_id, discord_message_id,
              results_message_id, created_at)
              VALUES (1, '2026-10-11T10:00:00Z', '2026-10-13T18:00:00Z', 'locked', '{3}', 2, '20', '21', '2026-10-11T10:00:00Z');
            ",
        )
        .unwrap();
        let plan = event_channel_purge(&mut conn, 1).unwrap().unwrap();
        assert_eq!(plan.discord_channel_id, "100");
        let mut keep: Vec<_> = plan.keep.into_iter().collect();
        keep.sort();
        assert_eq!(keep, vec!["10".to_string(), "20".to_string(), "21".to_string()]);
        assert!(event_channel_purge(&mut conn, 3).unwrap().is_none());
        assert!(event_channel_purge(&mut conn, 4).unwrap().is_none());
        diesel::update(schema::settings::table.filter(schema::settings::guild_id.eq(1)))
            .set(schema::settings::purge_channels.eq(false))
            .execute(&mut conn)
            .unwrap();
        assert!(event_channel_purge(&mut conn, 1).unwrap().is_none());
    }
}

/// An event's status as a post shows it, in `lang`.
pub fn status_text(lang: Lang, status: &str) -> String {
    match status {
        "draft" => tr!(lang, "status-draft"),
        "scheduled" => tr!(lang, "status-scheduled"),
        "posted" => tr!(lang, "status-posted"),
        "archived" => tr!(lang, "status-archived"),
        "cancelled" => tr!(lang, "status-cancelled"),
        "skipped" => tr!(lang, "status-skipped"),
        "failed" => tr!(lang, "status-failed"),
        _ => tr!(lang, "status-unknown"),
    }
}

pub fn status_label(status: &str) -> &'static str {
    match status {
        "draft" => "Draft",
        "scheduled" => "Scheduled",
        "posted" => "Posted",
        "archived" => "Archived",
        "cancelled" => "Cancelled",
        "skipped" => "Skipped",
        "failed" => "Post failed",
        _ => "Unknown",
    }
}
