use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection};

use super::db::{log, must_channel, must_event, must_guild, must_series};
use super::event::replace_children;
use super::model::*;
use super::signup::sync_access;
use crate::clock::{self, occurrence_start};
use crate::error::OpError;
use crate::store::{self, Event};

pub(super) fn link_new_series(
    conn: &Connection,
    event: &Event,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let Some(repeat) = &input.repeat else {
        return Ok(());
    };
    let settings = store::settings(conn, event.guild_id)?;
    let template = template_from(input, clean);
    let json = serde_json::to_string(&template)
        .map_err(|err| OpError::message(format!("Could not store the series: {err}")))?;
    conn.execute(
        "INSERT INTO series (guild_id, cadence, end_on, max_occurrences, active, template_json, anchor_start, post_lead_hours, created_at)
         VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?)",
        params![
            event.guild_id,
            repeat.cadence,
            repeat.end_on.map(|dt| clock::stamp(dt)),
            repeat.max_occurrences,
            json,
            clock::stamp(input.starts_at),
            settings.post_lead_hours,
            clock::stamp(now),
        ],
    )?;
    let series_id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE events SET series_id = ?, occurrence_index = 1 WHERE id = ?",
        params![series_id, event.id],
    )?;
    Ok(())
}

fn template_from(input: &EventInput, clean: &CleanInput) -> SeriesTemplate {
    let cutoff_offset_minutes = input.cutoff_at.and_then(|cutoff| {
        let mins = (input.starts_at - cutoff).num_minutes();
        if mins > 0 {
            Some(mins)
        } else {
            None
        }
    });
    let options = clean
        .options
        .iter()
        .cloned()
        .map(|mut opt| {
            opt.id = None;
            opt
        })
        .collect();
    SeriesTemplate {
        title: clean.title.clone(),
        description: clean.description.clone(),
        duration_minutes: input.duration_minutes,
        channel_id: input.channel_id,
        color: input.color.clone(),
        image_url: clean.image_url.clone(),
        thread_enabled: input.thread_enabled
            || input.vote.as_ref().is_some_and(|vote| vote.enabled),
        multiple_signups: input.multiple_signups,
        mention_everyone: input.mention_everyone,
        attendee_role_id: input.attendee_role_id,
        mention_role_ids: input.mention_role_ids.clone(),
        restrict_role_ids: input.restrict_role_ids.clone(),
        cutoff_offset_minutes,
        options,
        reminders: clean.reminders.clone(),
        vote: input.vote.clone().unwrap_or_default(),
    }
}

pub(super) fn apply_series_template(
    conn: &Connection,
    series_id: i64,
    source_id: i64,
    input: &EventInput,
    clean: &CleanInput,
) -> Result<(), OpError> {
    let mut template = template_from(input, clean);
    if input.vote.is_none() {
        let series = must_series(conn, series_id)?;
        if let Ok(old) = serde_json::from_str::<SeriesTemplate>(&series.template_json) {
            template.vote = old.vote;
        }
    }
    let json = serde_json::to_string(&template).map_err(|err| OpError::message(err.to_string()))?;
    conn.execute(
        "UPDATE series SET template_json = ? WHERE id = ?",
        params![json, series_id],
    )?;
    let events = store::events_in_series(conn, series_id)?;
    for event in events {
        if event.id == source_id || !matches!(event.status.as_str(), "draft" | "scheduled") {
            continue;
        }
        apply_template_to_event(conn, &event, &template)?;
    }
    Ok(())
}

fn apply_template_to_event(
    conn: &Connection,
    event: &Event,
    template: &SeriesTemplate,
) -> Result<(), OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| clock::stamp(start - Duration::minutes(mins)));
    conn.execute(
        "UPDATE events SET
            channel_id = ?, title = ?, description = ?, duration_minutes = ?, color = ?, image_url = ?,
            cutoff_at = ?, mention_everyone = ?, attendee_role_id = ?, thread_enabled = ?, multiple_signups = ?,
            updated_at = ?
         WHERE id = ?",
        params![
            template.channel_id,
            template.title,
            template.description,
            template.duration_minutes,
            template.color,
            template.image_url,
            cutoff,
            store::i(template.mention_everyone),
            template.attendee_role_id,
            store::i(template.thread_enabled),
            store::i(template.multiple_signups),
            store::now_stamp(),
            event.id,
        ],
    )?;
    replace_children(conn, event.id, &template.options, &template.reminders)?;
    conn.execute("DELETE FROM event_roles WHERE event_id = ?", [event.id])?;
    for role_id in &template.mention_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'mention')",
            params![event.id, role_id],
        )?;
    }
    for role_id in &template.restrict_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'restrict')",
            params![event.id, role_id],
        )?;
    }
    Ok(())
}

pub(super) fn ensure_series(
    conn: &Connection,
    series_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let series = must_series(conn, series_id)?;
    if !series.active {
        return Ok(());
    }
    let template: SeriesTemplate = serde_json::from_str(&series.template_json)
        .map_err(|err| OpError::message(format!("The series template is unreadable: {err}")))?;
    if template.vote.enabled {
        return Ok(());
    }
    let existing = store::events_in_series(conn, series_id)?;
    if !existing
        .iter()
        .any(|event| matches!(event.status.as_str(), "posted" | "scheduled" | "archived"))
    {
        return Ok(());
    }
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let anchor = clock::parse_stamp(&series.anchor_start)?;
    let have: HashSet<i64> = existing
        .iter()
        .filter_map(|event| event.occurrence_index)
        .collect();
    let creator = existing.first().map(|event| event.created_by).unwrap_or(0);
    let mut scheduled = existing
        .iter()
        .filter(|event| matches!(event.status.as_str(), "scheduled" | "draft"))
        .count();
    let mut index = 1_i64;
    let mut last_start = None;
    loop {
        if let Some(max) = series.max_occurrences {
            if index > max {
                break;
            }
        }
        let start = occurrence_start(anchor, index, &series.cadence, tz);
        if last_start.is_some_and(|previous| start <= previous) {
            break;
        }
        last_start = Some(start);
        if let Some(end) = &series.end_on {
            if start > clock::parse_stamp(end)? {
                break;
            }
        }
        if series.max_occurrences.is_none() && start > now + Duration::days(120) {
            break;
        }
        if !have.contains(&index) {
            if series.max_occurrences.is_none() && scheduled >= 8 {
                break;
            }
            insert_occurrence(conn, &series, &template, creator, index, start, tz)?;
            scheduled += 1;
        }
        index += 1;
    }
    Ok(())
}

pub(super) fn insert_occurrence(
    conn: &Connection,
    series: &store::Series,
    template: &SeriesTemplate,
    creator: i64,
    index: i64,
    start: DateTime<Utc>,
    _tz: Tz,
) -> Result<i64, OpError> {
    let post_at = start - Duration::hours(series.post_lead_hours);
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| start - Duration::minutes(mins));
    let stamp = store::now_stamp();
    conn.execute(
        "INSERT INTO events (
            public_id, guild_id, channel_id, series_id, occurrence_index, title, description, starts_at,
            duration_minutes, status, post_at, color, image_url, cutoff_at, mention_everyone,
            attendee_role_id, thread_enabled, multiple_signups, created_by, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'scheduled', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            crate::ids::event_id(),
            series.guild_id,
            template.channel_id,
            series.id,
            index,
            template.title,
            template.description,
            clock::stamp(start),
            template.duration_minutes,
            clock::stamp(post_at),
            template.color,
            template.image_url,
            cutoff.map(|dt| clock::stamp(dt)),
            store::i(template.mention_everyone),
            template.attendee_role_id,
            store::i(template.thread_enabled),
            store::i(template.multiple_signups),
            creator,
            stamp,
            stamp,
        ],
    )?;
    let id = conn.last_insert_rowid();
    replace_children(conn, id, &template.options, &template.reminders)?;
    for role_id in &template.mention_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'mention')",
            params![id, role_id],
        )?;
    }
    for role_id in &template.restrict_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'restrict')",
            params![id, role_id],
        )?;
    }
    Ok(id)
}

pub(super) fn mark_posted(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let event = must_event(conn, id)?;
    if event.status == "posted" {
        return Ok(());
    }
    let stamp = clock::stamp(now);
    conn.execute(
        "UPDATE events SET status = 'posted', posted_at = ?, post_attempts = 0, updated_at = ? WHERE id = ?",
        params![stamp, stamp, id],
    )?;
    let event = must_event(conn, id)?;
    if event.thread_enabled {
        conn.execute(
            "INSERT OR IGNORE INTO threads (event_id, name) VALUES (?, ?)",
            params![id, event.title],
        )?;
    }
    let settings = store::settings(conn, event.guild_id)?;
    if settings.calendar_auto_sync {
        conn.execute(
            "UPDATE events SET calendar_synced_at = ? WHERE id = ?",
            params![stamp, id],
        )?;
        log(
            conn,
            event.guild_id,
            Some(id),
            &format!("Synced {} to the calendar feed.", event.title),
        )?;
    }
    let channel = must_channel(conn, event.channel_id)?;
    log(
        conn,
        event.guild_id,
        Some(id),
        &format!("Posted {} in #{}.", event.title, channel.name),
    )?;
    sync_access(conn, id)?;
    Ok(())
}
