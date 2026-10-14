use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use super::access::{can_create, can_edit};
use super::db::{log, must_channel, must_event, must_role, must_user};
use super::model::*;
use super::series::{apply_series_template, ensure_series, link_new_series, mark_posted};
use super::signup::{fill_all, sync_access};
use super::template;
use crate::clock;
use crate::error::OpError;
use crate::store;

pub fn save_event(conn: &mut Connection, input: EventInput) -> Result<i64, OpError> {
    let clean = normalize(conn, &input)?;
    let now = Utc::now();
    store::with_tx(conn, |tx| {
        let id = if let Some(existing) = &clean.existing {
            update_event_row(tx, existing.id, &input, &clean, now)?;
            if existing.status == "posted" {
                update_options_in_place(tx, existing.id, &clean.options)?;
                replace_reminders(tx, existing.id, &clean.reminders)?;
            } else {
                replace_children(tx, existing.id, &clean.options, &clean.reminders)?;
            }
            replace_roles(tx, existing.id, &input)?;
            existing.id
        } else {
            let id = insert_event_row(tx, &input, &clean, now)?;
            replace_children(tx, id, &clean.options, &clean.reminders)?;
            replace_roles(tx, id, &input)?;
            id
        };

        transition(tx, id, &input, now)?;

        let event = must_event(tx, id)?;
        if input.repeat.is_some()
            && event.series_id.is_none()
            && clean.existing.as_ref().and_then(|e| e.series_id).is_none()
        {
            link_new_series(tx, &event, &input, &clean, now)?;
        }
        let event = must_event(tx, id)?;
        if input.apply_series {
            if let Some(series_id) = event.series_id {
                apply_series_template(tx, series_id, id, &input, &clean)?;
            }
        }
        let event = must_event(tx, id)?;
        if let Some(series_id) = event.series_id {
            if matches!(event.status.as_str(), "posted" | "scheduled" | "archived") {
                ensure_series(tx, series_id, now)?;
            }
        }
        if event.status == "posted" {
            fill_all(tx, id)?;
            sync_access(tx, id)?;
        }
        Ok(id)
    })
}

fn normalize(conn: &Connection, input: &EventInput) -> Result<CleanInput, OpError> {
    let settings = store::settings(conn, input.guild_id)?;
    let actor = must_user(conn, input.actor_id)?;
    if actor.guild_id != input.guild_id {
        return Err(OpError::message("That person isn't in this server."));
    }
    let roles = store::user_role_ids(conn, actor.id)?;
    let existing = if let Some(id) = input.id {
        let event = must_event(conn, id)?;
        if event.guild_id != input.guild_id {
            return Err(OpError::NotFound);
        }
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message(
                "You can edit events you created, or any event if you run the server.",
            ));
        }
        if matches!(event.status.as_str(), "archived" | "skipped" | "cancelled") {
            return Err(OpError::message(
                "That date is already closed. Duplicate it if you need another one.",
            ));
        }
        Some(event)
    } else {
        if !can_create(&actor, &settings, &roles) {
            return Err(OpError::message(
                "You don't have the role required to create events.",
            ));
        }
        None
    };

    let title = input.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(OpError::message(
            "Give the event a title, up to 120 characters.",
        ));
    }
    let description = input.description.trim().to_string();
    if description.chars().count() > 2000 {
        return Err(OpError::message(
            "Keep the description under 2000 characters.",
        ));
    }
    if !(15..=1440).contains(&input.duration_minutes) {
        return Err(OpError::message(
            "Duration needs to be between 15 minutes and 24 hours.",
        ));
    }
    if !valid_color(&input.color) {
        return Err(OpError::message("Pick one of the event colors."));
    }
    let image_url = match input
        .image_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(url)
            if (url.starts_with("https://") || url.starts_with("http://")) && url.len() <= 500 =>
        {
            Some(url.to_string())
        }
        Some(_) => {
            return Err(OpError::message(
                "Image links need to start with http:// or https://.",
            ))
        }
    };
    let channel = must_channel(conn, input.channel_id)?;
    if channel.guild_id != input.guild_id {
        return Err(OpError::message("Pick a channel from this server."));
    }
    if let Some(role_id) = input.attendee_role_id {
        let role = must_role(conn, role_id)?;
        if role.guild_id != input.guild_id {
            return Err(OpError::message(
                "The attendee role isn't from this server.",
            ));
        }
    }
    for role_id in input
        .mention_role_ids
        .iter()
        .chain(input.restrict_role_ids.iter())
    {
        let role = must_role(conn, *role_id)?;
        if role.guild_id != input.guild_id {
            return Err(OpError::message("A selected role isn't from this server."));
        }
    }
    if let Some(cutoff) = input.cutoff_at {
        if cutoff > input.starts_at {
            return Err(OpError::message(
                "The signup cutoff has to be before the event starts.",
            ));
        }
    }

    let mut options = Vec::new();
    for opt in &input.options {
        let name = opt.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if name.chars().count() > 40 {
            return Err(OpError::message(
                "Signup option names stay under 40 characters.",
            ));
        }
        let emoji = {
            let raw = opt.emoji.trim();
            let raw = if raw.is_empty() { "•" } else { raw };
            if raw.chars().count() > 8 {
                return Err(OpError::message("Keep each option emoji short."));
            }
            raw.to_string()
        };
        if let Some(cap) = opt.capacity {
            if cap < 1 || cap > 500 {
                return Err(OpError::message(
                    "Capacity has to be at least 1, or left open.",
                ));
            }
        }
        if opt.waitlist && opt.capacity.is_none() {
            return Err(OpError::message(format!(
                "{name} has a waitlist but no capacity. Set a cap, or turn the waitlist off."
            )));
        }
        if let Some(role_id) = opt.role_id {
            let role = must_role(conn, role_id)?;
            if role.guild_id != input.guild_id {
                return Err(OpError::message(
                    "An option is limited to a role from another server.",
                ));
            }
        }
        options.push(OptionInput {
            id: opt.id,
            name,
            emoji,
            capacity: opt.capacity,
            waitlist: opt.waitlist,
            role_id: opt.role_id,
            reminders_enabled: opt.reminders_enabled,
        });
    }
    if options.is_empty() {
        return Err(OpError::message("Add at least one signup option."));
    }
    if options.len() > 8 {
        return Err(OpError::message(
            "An event can have up to 8 signup options.",
        ));
    }
    if let Some(existing) = &existing {
        if existing.status == "posted" {
            let current = store::options_for(conn, existing.id)?;
            let ids: HashSet<i64> = current.iter().map(|opt| opt.id).collect();
            let same = options.len() == current.len()
                && options
                    .iter()
                    .all(|opt| opt.id.map(|id| ids.contains(&id)).unwrap_or(false));
            if !same {
                return Err(OpError::message(
                    "Signup options can't be added or removed after the event is posted. You can still rename them and change capacity.",
                ));
            }
        }
    }

    let mut reminders = Vec::new();
    for rem in &input.reminders {
        if rem.offset_minutes == 0 && rem.message.trim().is_empty() {
            continue;
        }
        if !(1..=10_080).contains(&rem.offset_minutes) {
            return Err(OpError::message(
                "Reminders need to be between 1 minute and 7 days out.",
            ));
        }
        if rem.message.chars().count() > 300 {
            return Err(OpError::message(
                "Keep reminder notes under 300 characters.",
            ));
        }
        reminders.push(ReminderInput {
            offset_minutes: rem.offset_minutes,
            message: rem.message.trim().to_string(),
        });
    }
    if reminders.is_empty() && !matches!(input.action, SaveAction::Draft) {
        reminders.push(ReminderInput {
            offset_minutes: settings.default_reminder_minutes.clamp(1, 10_080),
            message: String::new(),
        });
    }
    if reminders.len() > 5 {
        return Err(OpError::message("An event can have up to 5 reminders."));
    }

    if let Some(repeat) = &input.repeat {
        if !matches!(
            repeat.cadence.as_str(),
            "daily" | "weekly" | "biweekly" | "monthly"
        ) {
            return Err(OpError::message(
                "Repeat daily, weekly, every two weeks, or monthly.",
            ));
        }
        if let Some(max) = repeat.max_occurrences {
            if max < 2 {
                return Err(OpError::message(
                    "A series needs at least 2 dates. Leave the count empty to keep going.",
                ));
            }
        }
        if let Some(end) = repeat.end_on {
            if end < input.starts_at {
                return Err(OpError::message(
                    "The series end date is before the first event.",
                ));
            }
        }
    }
    if matches!(input.action, SaveAction::Schedule) {
        let post_at = input
            .post_at
            .ok_or_else(|| OpError::message("Pick a time for Marian to post this."))?;
        if post_at <= Utc::now() {
            return Err(OpError::message(
                "That post time is already past. Post it now, or pick a later time.",
            ));
        }
        if post_at >= input.starts_at {
            return Err(OpError::message("Post the event before it starts."));
        }
    }

    Ok(CleanInput {
        title,
        description,
        image_url,
        options,
        reminders,
        existing,
    })
}

fn valid_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}

fn insert_event_row(
    conn: &Connection,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<i64, OpError> {
    let stamp = clock::stamp(now);
    conn.execute(
        "INSERT INTO events (
            public_id, guild_id, channel_id, title, description, starts_at, duration_minutes, status,
            color, image_url, cutoff_at, mention_everyone, attendee_role_id, thread_enabled,
            multiple_signups, created_by, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 'draft', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            crate::ids::event_id(),
            input.guild_id,
            input.channel_id,
            template::fill_text(conn, input.guild_id, &clean.title, input.starts_at)?,
            template::fill_text(conn, input.guild_id, &clean.description, input.starts_at)?,
            clock::stamp(input.starts_at),
            input.duration_minutes,
            input.color,
            clean.image_url,
            input.cutoff_at.map(|dt| clock::stamp(dt)),
            store::i(input.mention_everyone),
            input.attendee_role_id,
            store::i(input.thread_enabled),
            store::i(input.multiple_signups),
            input.actor_id,
            stamp,
            stamp,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn update_event_row(
    conn: &Connection,
    id: i64,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE events SET
            channel_id = ?, title = ?, description = ?, starts_at = ?, duration_minutes = ?,
            color = ?, image_url = ?, cutoff_at = ?, mention_everyone = ?, attendee_role_id = ?,
            thread_enabled = ?, multiple_signups = ?, updated_at = ?
         WHERE id = ?",
        params![
            input.channel_id,
            template::fill_text(conn, input.guild_id, &clean.title, input.starts_at)?,
            template::fill_text(conn, input.guild_id, &clean.description, input.starts_at)?,
            clock::stamp(input.starts_at),
            input.duration_minutes,
            input.color,
            clean.image_url,
            input.cutoff_at.map(|dt| clock::stamp(dt)),
            store::i(input.mention_everyone),
            input.attendee_role_id,
            store::i(input.thread_enabled),
            store::i(input.multiple_signups),
            clock::stamp(now),
            id,
        ],
    )?;
    Ok(())
}

pub(super) fn replace_children(
    conn: &Connection,
    event_id: i64,
    options: &[OptionInput],
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    conn.execute("DELETE FROM signups WHERE event_id = ?", [event_id])?;
    conn.execute("DELETE FROM signup_options WHERE event_id = ?", [event_id])?;
    conn.execute("DELETE FROM reminders WHERE event_id = ?", [event_id])?;
    insert_options(conn, event_id, options)?;
    insert_reminders(conn, event_id, reminders, &[])?;
    Ok(())
}

fn update_options_in_place(
    conn: &Connection,
    event_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    for (index, opt) in options.iter().enumerate() {
        let Some(id) = opt.id else {
            return Err(OpError::message("A posted option is missing its id."));
        };
        conn.execute(
            "UPDATE signup_options SET name = ?, emoji = ?, capacity = ?, waitlist = ?, role_id = ?, reminders_enabled = ?, position = ?
             WHERE id = ? AND event_id = ?",
            params![
                opt.name,
                opt.emoji,
                opt.capacity,
                store::i(opt.waitlist),
                opt.role_id,
                store::i(opt.reminders_enabled),
                index as i64,
                id,
                event_id,
            ],
        )?;
    }
    Ok(())
}

fn replace_reminders(
    conn: &Connection,
    event_id: i64,
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    let old = store::reminders_for(conn, event_id)?;
    conn.execute("DELETE FROM reminders WHERE event_id = ?", [event_id])?;
    insert_reminders(conn, event_id, reminders, &old)?;
    Ok(())
}

fn insert_options(
    conn: &Connection,
    event_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    for (index, opt) in options.iter().enumerate() {
        conn.execute(
            "INSERT INTO signup_options (event_id, name, emoji, capacity, waitlist, role_id, reminders_enabled, position)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                opt.name,
                opt.emoji,
                opt.capacity,
                store::i(opt.waitlist),
                opt.role_id,
                store::i(opt.reminders_enabled),
                index as i64,
            ],
        )?;
    }
    Ok(())
}

fn insert_reminders(
    conn: &Connection,
    event_id: i64,
    reminders: &[ReminderInput],
    old: &[store::Reminder],
) -> Result<(), OpError> {
    for rem in reminders {
        let sent = old
            .iter()
            .find(|item| item.offset_minutes == rem.offset_minutes && item.message == rem.message)
            .and_then(|item| item.sent_at.clone());
        conn.execute(
            "INSERT INTO reminders (event_id, offset_minutes, message, sent_at) VALUES (?, ?, ?, ?)",
            params![event_id, rem.offset_minutes, rem.message, sent],
        )?;
    }
    Ok(())
}

fn replace_roles(conn: &Connection, event_id: i64, input: &EventInput) -> Result<(), OpError> {
    conn.execute("DELETE FROM event_roles WHERE event_id = ?", [event_id])?;
    for role_id in &input.mention_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'mention')",
            params![event_id, role_id],
        )?;
    }
    for role_id in &input.restrict_role_ids {
        conn.execute(
            "INSERT OR IGNORE INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'restrict')",
            params![event_id, role_id],
        )?;
    }
    Ok(())
}

fn transition(
    conn: &Connection,
    id: i64,
    input: &EventInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let event = must_event(conn, id)?;
    let stamp = clock::stamp(now);
    match (event.status.as_str(), input.action) {
        ("posted", _) => {
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!(
                    "Updated {}. The posted message now shows the new details.",
                    event.title
                ),
            )?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Publish) => {
            mark_posted(conn, id, now)?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Schedule)
        | ("scheduled", SaveAction::Save) => {
            let post_at = input
                .post_at
                .ok_or_else(|| OpError::message("Pick a time for Marian to post this."))?;
            conn.execute(
                "UPDATE events SET status = 'scheduled', post_at = ?, updated_at = ? WHERE id = ?",
                params![clock::stamp(post_at), stamp, id],
            )?;
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!("Will post {} at {}.", event.title, clock::stamp(post_at)),
            )?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Draft | SaveAction::Save) => {
            conn.execute(
                "UPDATE events SET status = 'draft', post_at = NULL, updated_at = ? WHERE id = ?",
                params![stamp, id],
            )?;
        }
        _ => {
            return Err(OpError::message("That event can't take this change."));
        }
    }
    Ok(())
}
