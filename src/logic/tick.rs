use std::collections::HashSet;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection};

use super::db::{log, must_event, must_guild, must_user};
use super::lifecycle::archive_tx;
use super::model::*;
use super::series::{ensure_series, mark_posted};
use super::signup::cutoff_passed;
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::store::{self, Event};

pub fn tick(conn: &mut Connection, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = TickReport {
        notes: Vec::new(),
        jobs: Vec::new(),
    };
    let guilds = store::list_guilds(conn)?;
    for guild in guilds {
        let piece = store::with_tx(conn, |tx| tick_guild(tx, guild.id, now))?;
        report.notes.extend(piece.notes);
        report.jobs.extend(piece.jobs);
    }
    Ok(report)
}

fn tick_guild(conn: &Connection, guild_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = TickReport {
        notes: Vec::new(),
        jobs: Vec::new(),
    };
    let settings = store::settings(conn, guild_id)?;
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let events = store::list_events(conn, guild_id)?;

    let mut posted_this_pass = 0;
    for event in &events {
        if posted_this_pass >= 3 {
            break;
        }
        if event.status != "scheduled" || event.post_attempts >= 3 {
            continue;
        }
        let Some(post_at) = event
            .post_at
            .as_deref()
            .and_then(|value| clock::parse_stamp(value).ok())
        else {
            continue;
        };
        if post_at > now {
            continue;
        }
        if store::channel(conn, event.channel_id)?.is_none() {
            fail_post(conn, event)?;
            report
                .notes
                .push(format!("Post failed for {}", event.title));
        } else {
            mark_posted(conn, event.id, now)?;
            report.jobs.push(DiscordJob::sync(event.id));
            report.jobs.push(DiscordJob::attend(event.id));
            report.notes.push(format!("Posted {}", event.title));
            posted_this_pass += 1;
        }
    }

    report
        .jobs
        .extend(super::routine::maintain(conn, guild_id, now)?);

    let events = store::list_events(conn, guild_id)?;
    for event in &events {
        if event.status != "posted" {
            continue;
        }
        let start = clock::parse_stamp(&event.starts_at)?;
        let reminders = store::reminders_for(conn, event.id)?;
        for rem in reminders {
            if rem.sent_at.is_some() {
                continue;
            }
            let due = start - Duration::minutes(rem.offset_minutes);
            if now < due {
                continue;
            }
            let stamp = clock::stamp(now);
            conn.execute(
                "UPDATE reminders SET sent_at = ? WHERE id = ?",
                params![stamp, rem.id],
            )?;
            if now > start + Duration::minutes(30) {
                let note = format!("Missed the reminder window for {}.", event.title);
                log(conn, guild_id, Some(event.id), &note)?;
                report.notes.push(note);
                continue;
            }
            let text = reminder_text(conn, event, &rem)?;
            match text {
                Some(text) => {
                    let place = if event.thread_enabled && settings.thread_reminders {
                        "thread"
                    } else {
                        "channel"
                    };
                    conn.execute(
                        "INSERT INTO thread_messages (event_id, body, place, created_at) VALUES (?, ?, ?, ?)",
                        params![event.id, text, place, stamp],
                    )?;
                    let note = format!(
                        "Reminder sent for {}: {} before.",
                        event.title,
                        clock::human_offset(rem.offset_minutes)
                    );
                    log(conn, guild_id, Some(event.id), &note)?;
                    report.jobs.push(DiscordJob::remind(event.id, text));
                    report.notes.push(note);
                }
                None => {
                    let note = format!(
                        "Skipped the {} reminder for {} — nobody to ping.",
                        clock::human_offset(rem.offset_minutes),
                        event.title
                    );
                    log(conn, guild_id, Some(event.id), &note)?;
                    report.notes.push(note);
                }
            }
        }
        if !event.signups_closed && cutoff_passed(event, now) {
            conn.execute(
                "UPDATE events SET signups_closed = 1, updated_at = ? WHERE id = ?",
                params![clock::stamp(now), event.id],
            )?;
            let note = format!("Signups for {} are closed.", event.title);
            log(conn, guild_id, Some(event.id), &note)?;
            report.notes.push(note);
            report.jobs.push(DiscordJob::sync(event.id));
            report.jobs.push(DiscordJob::attend(event.id));
        }
        if settings.auto_archive {
            let end = start + Duration::minutes(event.duration_minutes);
            if now >= end + Duration::hours(settings.auto_archive_hours) {
                let fresh = must_event(conn, event.id)?;
                if fresh.status == "posted" {
                    archive_tx(conn, &fresh, &settings)?;
                    report.jobs.push(DiscordJob::sync(event.id));
                    report.jobs.push(DiscordJob::attend(event.id));
                    report.notes.push(format!("Archived {}", event.title));
                }
            }
        }
    }

    for series in store::list_series(conn, guild_id)? {
        if series.active {
            ensure_series(conn, series.id, now)?;
        }
    }

    if settings.digest == "daily" || settings.digest == "weekly" {
        let local = now.with_timezone(&tz);
        let due_day = settings.digest == "daily" || local.weekday() == chrono::Weekday::Mon;
        if due_day && local.time().hour() >= 9 {
            let key = format!("digest:{guild_id}:{}", local.date_naive());
            if store::meta(conn, &key)?.is_none() {
                let events = store::list_events(conn, guild_id)?;
                let mut names = Vec::new();
                for event in &events {
                    if event.status != "posted" {
                        continue;
                    }
                    let start = clock::parse_stamp(&event.starts_at)?;
                    if start.with_timezone(&tz).date_naive() == local.date_naive() {
                        names.push(format!(
                            "{} at {}",
                            event.title,
                            clock::format_clock(start.with_timezone(&tz), settings.hour_24)
                        ));
                    }
                }
                let note = if names.is_empty() {
                    "Daily digest: nothing scheduled today.".to_string()
                } else {
                    format!("Daily digest: {}.", names.join(", "))
                };
                log(conn, guild_id, None, &note)?;
                store::set_meta(conn, &key, "1")?;
                report.notes.push(note);
            }
        }
    }
    Ok(report)
}

fn fail_post(conn: &Connection, event: &Event) -> Result<(), OpError> {
    let attempts = event.post_attempts + 1;
    let stamp = store::now_stamp();
    if attempts >= 3 {
        conn.execute(
            "UPDATE events SET status = 'failed', post_attempts = ?, updated_at = ? WHERE id = ?",
            params![attempts, stamp, event.id],
        )?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!(
                "Couldn't post {} after 3 tries. Post it from the event page.",
                event.title
            ),
        )?;
    } else {
        conn.execute(
            "UPDATE events SET post_attempts = ?, updated_at = ? WHERE id = ?",
            params![attempts, stamp, event.id],
        )?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!("Posting {} failed. Marian will try again.", event.title),
        )?;
    }
    Ok(())
}

fn reminder_text(
    conn: &Connection,
    event: &Event,
    rem: &store::Reminder,
) -> Result<Option<String>, OpError> {
    let options = store::options_for(conn, event.id)?;
    let reminding: HashSet<i64> = options
        .iter()
        .filter(|opt| opt.reminders_enabled)
        .map(|opt| opt.id)
        .collect();
    let signups = store::signups_for(conn, event.id)?;
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();
    for signup in signups {
        if signup.state != "confirmed"
            || !reminding.contains(&signup.option_id)
            || !seen.insert(signup.user_id)
        {
            continue;
        }
        let user = must_user(conn, signup.user_id)?;
        if !user.reminders_enabled {
            continue;
        }
        if let Some(discord_id) = &user.discord_id {
            mentions.push(format!("<@{discord_id}>"));
        } else {
            mentions.push(user.name);
        }
    }
    if mentions.is_empty() {
        return Ok(None);
    }
    let mut text = format!(
        "{} — **{}** starts in {}.",
        mentions.join(" "),
        event.title,
        clock::human_offset(rem.offset_minutes)
    );
    if !rem.message.trim().is_empty() {
        text.push(' ');
        text.push_str(rem.message.trim());
    }
    Ok(Some(text))
}

pub fn rebase(conn: &mut Connection, now: DateTime<Utc>) -> Result<(), OpError> {
    let guilds = store::list_guilds(conn)?;
    for guild in guilds {
        let settings = store::settings(conn, guild.id)?;
        if !settings.park_clock {
            continue;
        }
        let key = format!("anchor:{}", guild.id);
        let Some(anchor_s) = store::meta(conn, &key)? else {
            store::set_meta(conn, &key, &clock::stamp(now))?;
            continue;
        };
        let anchor = clock::parse_stamp(&anchor_s)?;
        let delta = now - anchor;
        if delta.num_seconds().abs() < 2 {
            continue;
        }
        shift_guild(conn, guild.id, delta)?;
        store::set_meta(conn, &key, &clock::stamp(now))?;
    }
    Ok(())
}

fn shift_guild(conn: &mut Connection, guild_id: i64, delta: Duration) -> Result<(), OpError> {
    store::with_tx(conn, |tx| {
        let events = store::list_events(tx, guild_id)?;
        for event in events {
            tx.execute(
                "UPDATE events SET starts_at=?, post_at=?, posted_at=?, archived_at=?, cutoff_at=?, created_at=?, updated_at=?, calendar_synced_at=? WHERE id=?",
                params![
                    moved(&event.starts_at, delta)?,
                    moved_opt(event.post_at, delta)?,
                    moved_opt(event.posted_at, delta)?,
                    moved_opt(event.archived_at, delta)?,
                    moved_opt(event.cutoff_at, delta)?,
                    moved(&event.created_at, delta)?,
                    moved(&event.updated_at, delta)?,
                    moved_opt(event.calendar_synced_at, delta)?,
                    event.id,
                ],
            )?;
            for rem in store::reminders_for(tx, event.id)? {
                if let Some(sent) = rem.sent_at {
                    tx.execute(
                        "UPDATE reminders SET sent_at = ? WHERE id = ?",
                        params![moved(&sent, delta)?, rem.id],
                    )?;
                }
            }
            for signup in store::signups_for(tx, event.id)? {
                tx.execute(
                    "UPDATE signups SET created_at = ? WHERE id = ?",
                    params![moved(&signup.created_at, delta)?, signup.id],
                )?;
            }
            for message in store::thread_messages(tx, event.id)? {
                tx.execute(
                    "UPDATE thread_messages SET created_at = ? WHERE id = ?",
                    params![moved(&message.created_at, delta)?, message.id],
                )?;
            }
        }
        for series in store::list_series(tx, guild_id)? {
            tx.execute(
                "UPDATE series SET anchor_start=?, end_on=?, created_at=? WHERE id=?",
                params![
                    moved(&series.anchor_start, delta)?,
                    moved_opt(series.end_on, delta)?,
                    moved(&series.created_at, delta)?,
                    series.id,
                ],
            )?;
        }
        let mut stmt = tx.prepare(
            "SELECT routine_runs.id, routine_runs.opens_at, routine_runs.locks_at, routine_runs.created_at
             FROM routine_runs
             JOIN routines ON routines.id = routine_runs.routine_id
             WHERE routines.guild_id = ?",
        )?;
        let runs: Vec<(i64, String, String, String)> = stmt
            .query_map([guild_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, opens, locks, created) in runs {
            tx.execute(
                "UPDATE routine_runs SET opens_at = ?, locks_at = ?, created_at = ? WHERE id = ?",
                params![
                    moved(&opens, delta)?,
                    moved(&locks, delta)?,
                    moved(&created, delta)?,
                    id
                ],
            )?;
        }
        for item in store::list_activity(tx, guild_id, 100_000)? {
            tx.execute(
                "UPDATE activity SET created_at = ? WHERE id = ?",
                params![moved(&item.created_at, delta)?, item.id],
            )?;
        }
        for row in store::list_commands(tx, guild_id, 200)? {
            tx.execute(
                "UPDATE command_log SET created_at = ? WHERE id = ?",
                params![moved(&row.created_at, delta)?, row.id],
            )?;
        }
        for channel in store::list_channels(tx, guild_id)? {
            if let Some(purged) = channel.purged_at {
                tx.execute(
                    "UPDATE channels SET purged_at = ? WHERE id = ?",
                    params![moved(&purged, delta)?, channel.id],
                )?;
            }
        }
        Ok(())
    })
}

fn moved(value: &str, delta: Duration) -> Result<String, OpError> {
    Ok(clock::stamp(clock::parse_stamp(value)? + delta))
}

fn moved_opt(value: Option<String>, delta: Duration) -> Result<Option<String>, OpError> {
    value.as_deref().map(|item| moved(item, delta)).transpose()
}
