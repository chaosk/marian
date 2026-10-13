use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};

use super::catalog::slug_channel;
use super::db::{must_channel, must_event, must_guild, trim_chars};
use super::model::*;
use crate::clock;
use crate::error::OpError;
use crate::store::{self, Channel, User};

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

pub fn attendance(conn: &Connection, event_id: i64) -> Result<Attendance, OpError> {
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
        return Ok(Attendance::Waiting);
    };
    let settings = store::settings(conn, event.guild_id)?;
    let manage_thread = event.thread_enabled && settings.auto_join_threads;
    let desired_thread = if manage_thread {
        store::discord_ids_for_users(conn, &store::thread_member_ids(conn, event_id)?)?
    } else {
        Vec::new()
    };
    let applied_thread = store::applied_user_ids(conn, event_id, "thread")?;
    let (thread_add, thread_remove) = id_diff(&desired_thread, &applied_thread);
    let grants = store::grant_user_ids(conn, event_id)?;
    let desired_role = store::discord_ids_for_users(conn, &grants)?;
    let applied_role = store::applied_user_ids(conn, event_id, "role")?;
    let (role_add, role_remove) = id_diff(&desired_role, &applied_role);
    let role = event
        .attendee_role_id
        .and_then(|id| store::role(conn, id).ok().flatten());
    Ok(Attendance::Ready(AttendanceWork {
        title: event.title.clone(),
        guild_discord_id,
        channel_discord_id,
        message_id,
        thread_discord_id: store::thread_discord_id(conn, event_id)?,
        thread_name: trim_chars(&event.title, 100),
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

pub fn discord_view(
    conn: &Connection,
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
    let start = clock::parse_stamp(&event.starts_at)?;
    let end = start + chrono::Duration::minutes(event.duration_minutes);
    let mut body = String::new();
    if !event.description.trim().is_empty() {
        body.push_str(event.description.trim());
        body.push_str("\n\n");
    }
    body.push_str(&format!(
        "**Time**\n<t:{}:F> - <t:{}:t>\n",
        start.timestamp(),
        end.timestamp()
    ));
    let plus = crate::ics::calendar_template(&event)?;
    let web = web_event_url(public_url, guild.discord_id.as_deref(), &event.public_id);
    match web.as_deref() {
        Some(url) => body.push_str(&format!("[+]({plus}) · [View on web]({url})\n")),
        None => body.push_str(&format!("[+]({plus})\n")),
    }
    body.push_str(&format!("🕐 <t:{}:R>", start.timestamp()));
    if event.status != "posted" {
        body.push_str(&format!("\n\n_{}_", status_label(&event.status)));
    }
    let options = store::options_for(conn, event.id)?;
    let signups = store::signups_for(conn, event.id)?;
    let users = store::list_users(conn, event.guild_id)?;
    let names: HashMap<i64, String> = users.into_iter().map(|user| (user.id, user.name)).collect();
    let mut fields = Vec::new();
    let mut buttons = Vec::new();
    let closed = event.status != "posted" || event.signups_closed;
    for opt in &options {
        let confirmed: Vec<_> = signups
            .iter()
            .filter(|signup| signup.option_id == opt.id && signup.state == "confirmed")
            .collect();
        let waiting = signups
            .iter()
            .filter(|signup| signup.option_id == opt.id && signup.state == "waitlisted")
            .count();
        let count = match opt.capacity {
            Some(cap) => format!("{}/{}", confirmed.len(), cap),
            None => confirmed.len().to_string(),
        };
        let mut lines = Vec::new();
        if settings.show_attendees {
            lines.extend(
                confirmed
                    .iter()
                    .filter_map(|signup| names.get(&signup.user_id))
                    .cloned(),
            );
        }
        if waiting > 0 {
            lines.push(format!("{waiting} waiting"));
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
            role: ButtonRole::Signup(opt.id),
            label: String::new(),
            emoji: opt.emoji.clone(),
            tone: ButtonTone::Secondary,
            disabled: closed,
        });
    }
    buttons.push(DiscordButton {
        role: ButtonRole::Edit,
        label: "Edit".into(),
        emoji: String::new(),
        tone: ButtonTone::Primary,
        disabled: false,
    });
    buttons.push(DiscordButton {
        role: ButtonRole::Delete,
        label: "Delete".into(),
        emoji: String::new(),
        tone: ButtonTone::Danger,
        disabled: false,
    });
    let creator = store::user(conn, event.created_by)?
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
        channel_id,
        message_id: event.discord_message_id.clone(),
        title: event.title,
        body,
        color: u32::from_str_radix(event.color.trim_start_matches('#'), 16).unwrap_or(0x1e3a6e),
        fields,
        footer: format!("Created by {creator}"),
        image_url,
        buttons,
    }))
}

fn web_event_url(public_url: &str, server: Option<&str>, public_id: &str) -> Option<String> {
    let base = public_url.trim().trim_end_matches('/');
    let server = server.filter(|id| !id.is_empty())?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/servers/{server}/events/{public_id}"))
}

pub fn remember_discord_message(
    conn: &Connection,
    event_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE events SET discord_message_id = ? WHERE id = ?",
        params![message_id, event_id],
    )?;
    Ok(())
}

pub fn ensure_discord_guild(
    conn: &mut Connection,
    discord_id: &str,
    name: &str,
) -> Result<store::Guild, OpError> {
    if let Some(guild) = store::guild_by_discord(conn, discord_id)? {
        if guild.name != name && !name.is_empty() {
            conn.execute(
                "UPDATE guilds SET name = ? WHERE id = ?",
                params![name, guild.id],
            )?;
        }
        return store::guild(conn, guild.id)?.ok_or(OpError::NotFound);
    }
    conn.execute(
        "INSERT INTO guilds (name, timezone, discord_id) VALUES (?, 'UTC', ?)",
        params![
            if name.is_empty() {
                "Discord server"
            } else {
                name
            },
            discord_id
        ],
    )?;
    let id = conn.last_insert_rowid();
    store::insert_default_settings(conn, id)?;
    store::guild(conn, id)?.ok_or(OpError::NotFound)
}

pub fn ensure_discord_user(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
    name: &str,
) -> Result<User, OpError> {
    if let Some(user) = store::user_by_discord(conn, guild_id, discord_id)? {
        if user.name != name && !name.is_empty() {
            conn.execute(
                "UPDATE users SET name = ? WHERE id = ?",
                params![name, user.id],
            )?;
        }
        return store::user(conn, user.id)?.ok_or(OpError::NotFound);
    }
    let existing = store::list_users(conn, guild_id)?;
    let organizer = existing.iter().all(|user| !user.organizer);
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
    conn.execute(
        "INSERT INTO users (guild_id, name, handle, color, reminders_enabled, organizer, discord_id)
         VALUES (?, ?, ?, ?, 1, ?, ?)",
        params![guild_id, display, handle, color, store::i(organizer), discord_id],
    )?;
    store::user(conn, conn.last_insert_rowid())?.ok_or(OpError::NotFound)
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
    let slug = {
        let raw = slug_channel(name);
        if raw.is_empty() {
            format!(
                "channel-{}",
                &discord_id[discord_id.len().saturating_sub(6)..]
            )
        } else {
            raw
        }
    };
    let slug = if store::channel_by_name(conn, guild_id, &slug)?.is_some() {
        format!(
            "{slug}-{}",
            &discord_id[discord_id.len().saturating_sub(4)..]
        )
    } else {
        slug
    };
    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM channels WHERE guild_id = ?",
        [guild_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO channels (guild_id, name, kind, position, discord_id) VALUES (?, ?, 'text', ?, ?)",
        params![guild_id, slug, position, discord_id],
    )?;
    store::channel(conn, conn.last_insert_rowid())?.ok_or(OpError::NotFound)
}

fn unique_handle(
    conn: &Connection,
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
    if store::user_by_handle(conn, guild_id, &base)?.is_none() {
        return Ok(base);
    }
    let suffix = &discord_id[discord_id.len().saturating_sub(4)..];
    Ok(format!("{base}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_card_lists_names_in_columns() {
        let conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO channels (id, guild_id, name, discord_id) VALUES (1, 1, 'social', '100');
            INSERT INTO users (id, guild_id, name, handle, color) VALUES
              (1, 1, 'Nell', 'nell', '#112233'),
              (2, 1, 'June', 'june', '#112233');
            INSERT INTO events (
              public_id, guild_id, channel_id, title, description, starts_at, duration_minutes,
              status, created_by, created_at, updated_at
            ) VALUES (
              '01900000-0000-7000-8000-000000000010', 1, 1, 'Shore cleanup', 'Bring gloves',
              '2026-10-14T18:00:00Z', 60, 'posted', 1, '2026-10-13T00:00:00Z', '2026-10-13T00:00:00Z'
            );
            INSERT INTO signup_options (id, event_id, name, emoji, position) VALUES
              (1, 1, 'Accepted', '✅', 0),
              (2, 1, 'Declined', '❌', 1);
            INSERT INTO signups (event_id, option_id, user_id, state, created_at) VALUES
              (1, 1, 1, 'confirmed', '2026-10-13T00:00:00Z'),
              (1, 1, 2, 'confirmed', '2026-10-13T00:00:00Z'),
              (1, 2, 2, 'waitlisted', '2026-10-13T00:00:00Z');
            ",
        )
        .unwrap();
        store::insert_default_settings(&conn, 1).unwrap();
        let view = discord_view(&conn, 1, "https://marian.example.com")
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
        assert_eq!(view.footer, "Created by Nell");
        assert_eq!(view.fields.len(), 2);
        assert!(view.fields.iter().all(|field| field.inline));
        assert_eq!(view.fields[0].name, "✅ **Accepted** (2)");
        assert_eq!(view.fields[0].value, "Nell\nJune");
        assert_eq!(view.fields[1].name, "❌ **Declined** (0)");
        assert_eq!(view.fields[1].value, "1 waiting");
        assert_eq!(view.buttons.len(), 4);
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
        "ballot" => "Voting",
        _ => "Unknown",
    }
}
