use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection};

use super::db::{log, must_channel, must_event, must_guild, must_series};
use super::event::replace_children;
use super::model::*;
use super::signup::sync_access;
use super::template::{self, Spawn};
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
    let template_id = template::insert(conn, &template::from_series(event.guild_id, &template))?;
    conn.execute(
        "INSERT INTO series (public_id, guild_id, cadence, end_on, max_occurrences, active, template_id, anchor_start, post_lead_hours, created_at)
         VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
        params![
            crate::ids::series_id(),
            event.guild_id,
            repeat.cadence,
            repeat.end_on.map(|dt| clock::stamp(dt)),
            repeat.max_occurrences,
            template_id,
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
        thread_enabled: input.thread_enabled,
        multiple_signups: input.multiple_signups,
        mention_everyone: input.mention_everyone,
        attendee_role_id: input.attendee_role_id,
        mention_role_ids: input.mention_role_ids.clone(),
        restrict_role_ids: input.restrict_role_ids.clone(),
        cutoff_offset_minutes,
        options,
        reminders: clean.reminders.clone(),
    }
}

pub(super) fn apply_series_template(
    conn: &Connection,
    series_id: i64,
    source_id: i64,
    input: &EventInput,
    clean: &CleanInput,
) -> Result<(), OpError> {
    let series = must_series(conn, series_id)?;
    let template = template_from(input, clean);
    template::update(
        conn,
        series.template_id,
        &template::from_series(series.guild_id, &template),
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
    let guild = must_guild(conn, event.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
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
            template::fill(&template.title, start, tz),
            template::fill(&template.description, start, tz),
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

pub(super) fn retune_series(
    conn: &Connection,
    series_id: i64,
    repeat: &RepeatSpec,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let series = must_series(conn, series_id)?;
    if !series.active {
        return Err(OpError::message("This series has ended."));
    }
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
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let anchor = clock::parse_stamp(&series.anchor_start)?;
    if let Some(end) = repeat.end_on {
        if end < anchor {
            return Err(OpError::message(
                "The series end date is before the first event.",
            ));
        }
    }
    let cadence_changed = series.cadence != repeat.cadence;
    conn.execute(
        "UPDATE series SET cadence = ?, end_on = ?, max_occurrences = ? WHERE id = ?",
        params![
            repeat.cadence,
            repeat.end_on.map(clock::stamp),
            repeat.max_occurrences,
            series_id,
        ],
    )?;
    for event in store::events_in_series(conn, series_id)? {
        let Some(index) = event.occurrence_index else {
            continue;
        };
        if !matches!(event.status.as_str(), "draft" | "scheduled" | "failed") {
            continue;
        }
        let mut start = clock::parse_stamp(&event.starts_at)?;
        if cadence_changed {
            let next = occurrence_start(anchor, index, &repeat.cadence, tz);
            if next != start {
                shift_occurrence(conn, &event, start, next, tz)?;
                start = next;
            }
        }
        let past_count = repeat.max_occurrences.is_some_and(|max| index > max);
        let past_end = repeat.end_on.is_some_and(|end| start > end);
        if past_count || past_end {
            conn.execute("DELETE FROM events WHERE id = ?", [event.id])?;
        }
    }
    ensure_series(conn, series_id, now)?;
    Ok(())
}

fn shift_occurrence(
    conn: &Connection,
    event: &Event,
    old: DateTime<Utc>,
    new: DateTime<Utc>,
    tz: Tz,
) -> Result<(), OpError> {
    let delta = new - old;
    let post_at = event
        .post_at
        .as_deref()
        .map(|raw| Ok::<_, OpError>(clock::stamp(clock::parse_stamp(raw)? + delta)))
        .transpose()?;
    let cutoff = event
        .cutoff_at
        .as_deref()
        .map(|raw| Ok::<_, OpError>(clock::stamp(clock::parse_stamp(raw)? + delta)))
        .transpose()?;
    let old_day = clock::format_day(old.with_timezone(&tz).date_naive());
    let new_day = clock::format_day(new.with_timezone(&tz).date_naive());
    conn.execute(
        "UPDATE events SET title = ?, description = ?, starts_at = ?, post_at = ?, cutoff_at = ?, updated_at = ? WHERE id = ?",
        params![
            event.title.replace(&old_day, &new_day),
            event.description.replace(&old_day, &new_day),
            clock::stamp(new),
            post_at,
            cutoff,
            store::now_stamp(),
            event.id,
        ],
    )?;
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
    let template = template::series_template(conn, &series)?;
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
    tz: Tz,
) -> Result<i64, OpError> {
    let post_at = start - Duration::hours(series.post_lead_hours);
    let stored = template::from_series(series.guild_id, template);
    template::spawn(
        conn,
        &stored,
        &Spawn {
            created_by: creator,
            starts_at: start,
            post_at: Some(post_at),
            status: "scheduled",
            series_id: Some(series.id),
            occurrence_index: Some(index),
            tz,
        },
    )
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
