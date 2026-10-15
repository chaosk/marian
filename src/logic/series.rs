use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::{log, must_channel, must_event, must_guild, must_series};
use super::event::replace_children;
use super::model::*;
use super::signup::sync_access;
use super::template::{self, Spawn};
use crate::clock::{self, occurrence_start};
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event};

pub(super) fn link_new_series(
    conn: &mut Connection,
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
    let series_id: i64 = diesel::insert_into(schema::series::table)
        .values((
            schema::series::public_id.eq(crate::ids::series_id()),
            schema::series::guild_id.eq(event.guild_id),
            schema::series::cadence.eq(&repeat.cadence),
            schema::series::end_on.eq(repeat.end_on.map(|dt| clock::stamp(dt))),
            schema::series::max_occurrences.eq(repeat.max_occurrences),
            schema::series::active.eq(true),
            schema::series::template_id.eq(template_id),
            schema::series::anchor_start.eq(clock::stamp(input.starts_at)),
            schema::series::post_lead_hours.eq(settings.post_lead_hours),
            schema::series::created_at.eq(clock::stamp(now)),
        ))
        .returning(schema::series::id)
        .get_result(conn)?;
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::series_id.eq(series_id),
            schema::events::occurrence_index.eq(1_i64),
        ))
        .execute(conn)?;
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
    conn: &mut Connection,
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
    conn: &mut Connection,
    event: &Event,
    template: &SeriesTemplate,
) -> Result<(), OpError> {
    let start = clock::parse_stamp(&event.starts_at)?;
    let guild = must_guild(conn, event.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| clock::stamp(start - Duration::minutes(mins)));
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::channel_id.eq(template.channel_id),
            schema::events::title.eq(template::fill(&template.title, start, tz)),
            schema::events::description.eq(template::fill(&template.description, start, tz)),
            schema::events::duration_minutes.eq(template.duration_minutes),
            schema::events::color.eq(&template.color),
            schema::events::image_url.eq(template.image_url.as_deref()),
            schema::events::cutoff_at.eq(cutoff.as_deref()),
            schema::events::mention_everyone.eq(template.mention_everyone),
            schema::events::attendee_role_id.eq(template.attendee_role_id),
            schema::events::thread_enabled.eq(template.thread_enabled),
            schema::events::multiple_signups.eq(template.multiple_signups),
            schema::events::updated_at.eq(store::now_stamp()),
        ))
        .execute(conn)?;
    replace_children(conn, event.id, &template.options, &template.reminders)?;
    diesel::delete(schema::event_roles::table.filter(schema::event_roles::event_id.eq(event.id)))
        .execute(conn)?;
    for role_id in &template.mention_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(event.id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("mention"),
            ))
            .execute(conn)?;
    }
    for role_id in &template.restrict_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(event.id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("restrict"),
            ))
            .execute(conn)?;
    }
    Ok(())
}

pub(super) fn retune_series(
    conn: &mut Connection,
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
    diesel::update(schema::series::table.filter(schema::series::id.eq(series_id)))
        .set((
            schema::series::cadence.eq(&repeat.cadence),
            schema::series::end_on.eq(repeat.end_on.map(clock::stamp)),
            schema::series::max_occurrences.eq(repeat.max_occurrences),
        ))
        .execute(conn)?;
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
            diesel::delete(schema::events::table.filter(schema::events::id.eq(event.id)))
                .execute(conn)?;
        }
    }
    ensure_series(conn, series_id, now)?;
    Ok(())
}

fn shift_occurrence(
    conn: &mut Connection,
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
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::title.eq(event.title.replace(&old_day, &new_day)),
            schema::events::description.eq(event.description.replace(&old_day, &new_day)),
            schema::events::starts_at.eq(clock::stamp(new)),
            schema::events::post_at.eq(post_at),
            schema::events::cutoff_at.eq(cutoff),
            schema::events::updated_at.eq(store::now_stamp()),
        ))
        .execute(conn)?;
    Ok(())
}

pub(super) fn ensure_series(
    conn: &mut Connection,
    series_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let series = must_series(conn, series_id)?;
    let existing = store::events_in_series(conn, series_id)?;
    let guild = must_guild(conn, series.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    fill_series(conn, &series, &existing, tz, now)
}

/// `existing` is that series' dates, oldest occurrence first.
pub(super) fn fill_series(
    conn: &mut Connection,
    series: &store::Series,
    existing: &[Event],
    tz: Tz,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    if !series.active {
        return Ok(());
    }
    if !existing
        .iter()
        .any(|event| matches!(event.status.as_str(), "posted" | "scheduled" | "archived"))
    {
        return Ok(());
    }
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
    let mut template = None;
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
            if template.is_none() {
                template = Some(template::series_template(conn, series)?);
            }
            insert_occurrence(
                conn,
                series,
                template.as_ref().expect("template loaded above"),
                creator,
                index,
                start,
                tz,
            )?;
            scheduled += 1;
        }
        index += 1;
    }
    Ok(())
}

pub(super) fn insert_occurrence(
    conn: &mut Connection,
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

pub(super) fn mark_posted(
    conn: &mut Connection,
    id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let event = must_event(conn, id)?;
    if event.status == "posted" {
        return Ok(());
    }
    let stamp = clock::stamp(now);
    diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
        .set((
            schema::events::status.eq("posted"),
            schema::events::posted_at.eq(&stamp),
            schema::events::post_attempts.eq(0),
            schema::events::updated_at.eq(&stamp),
        ))
        .execute(conn)?;
    let event = must_event(conn, id)?;
    if event.thread_enabled {
        diesel::insert_into(schema::threads::table)
            .values((
                schema::threads::event_id.eq(id),
                schema::threads::name.eq(&super::discord::stored_thread_name(conn, &event)?),
            ))
            .on_conflict_do_nothing()
            .execute(conn)?;
    }
    let settings = store::settings(conn, event.guild_id)?;
    if settings.calendar_auto_sync {
        diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
            .set(schema::events::calendar_synced_at.eq(&stamp))
            .execute(conn)?;
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
