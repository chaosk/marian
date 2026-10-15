use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::{log, must_event, must_guild, must_user};
use super::lifecycle::archive_tx;
use super::model::*;
use super::series::{fill_series, mark_posted};
use super::signup::cutoff_passed;
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::schema;
use crate::store::{self, Connection, Event};

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

fn tick_guild(
    conn: &mut Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<TickReport, OpError> {
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

    let reminders = grouped(store::reminders_in_guild(conn, guild_id)?, |rem| {
        rem.event_id
    });
    let mut people = None;
    let events = store::list_events(conn, guild_id)?;
    for event in &events {
        if event.status != "posted" {
            continue;
        }
        let start = clock::parse_stamp(&event.starts_at)?;
        let reminders = reminders.get(&event.id).map(Vec::as_slice).unwrap_or(&[]);
        for rem in reminders {
            if rem.sent_at.is_some() {
                continue;
            }
            let due = start - Duration::minutes(rem.offset_minutes);
            if now < due {
                continue;
            }
            let stamp = clock::stamp(now);
            diesel::update(schema::reminders::table.filter(schema::reminders::id.eq(rem.id)))
                .set(schema::reminders::sent_at.eq(&stamp))
                .execute(conn)?;
            if now > start + Duration::minutes(30) {
                let note = format!("Missed the reminder window for {}.", event.title);
                log(conn, guild_id, Some(event.id), &note)?;
                report.notes.push(note);
                continue;
            }
            let cached = guild_people(conn, guild_id, &mut people)?;
            let text = reminder_text(conn, event, rem, cached)?;
            match text {
                Some(text) => {
                    let place = if event.thread_enabled && settings.thread_reminders {
                        "thread"
                    } else {
                        "channel"
                    };
                    diesel::insert_into(schema::thread_messages::table)
                        .values((
                            schema::thread_messages::event_id.eq(event.id),
                            schema::thread_messages::body.eq(&text),
                            schema::thread_messages::place.eq(place),
                            schema::thread_messages::created_at.eq(&stamp),
                        ))
                        .execute(conn)?;
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
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::signups_closed.eq(true),
                    schema::events::updated_at.eq(clock::stamp(now)),
                ))
                .execute(conn)?;
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

    let dates = grouped(store::events_in_active_series(conn, guild_id)?, |event| {
        event.series_id.unwrap_or(0)
    });
    for series in store::list_series(conn, guild_id)? {
        if series.active {
            let existing = dates.get(&series.id).map(Vec::as_slice).unwrap_or(&[]);
            fill_series(conn, &series, existing, tz, now)?;
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

fn fail_post(conn: &mut Connection, event: &Event) -> Result<(), OpError> {
    let attempts = event.post_attempts + 1;
    let stamp = store::now_stamp();
    if attempts >= 3 {
        diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
            .set((
                schema::events::status.eq("failed"),
                schema::events::post_attempts.eq(attempts),
                schema::events::updated_at.eq(&stamp),
            ))
            .execute(conn)?;
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
        diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
            .set((
                schema::events::post_attempts.eq(attempts),
                schema::events::updated_at.eq(&stamp),
            ))
            .execute(conn)?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!("Posting {} failed. Marian will try again.", event.title),
        )?;
    }
    Ok(())
}

fn guild_people<'a>(
    conn: &mut Connection,
    guild_id: i64,
    cache: &'a mut Option<HashMap<i64, store::User>>,
) -> Result<&'a HashMap<i64, store::User>, OpError> {
    if cache.is_none() {
        let loaded = store::list_users(conn, guild_id)?
            .into_iter()
            .map(|user| (user.id, user))
            .collect();
        *cache = Some(loaded);
    }
    Ok(cache.as_ref().expect("people loaded above"))
}

fn grouped<T>(rows: Vec<T>, key: impl Fn(&T) -> i64) -> HashMap<i64, Vec<T>> {
    let mut grouped: HashMap<i64, Vec<T>> = HashMap::new();
    for row in rows {
        grouped.entry(key(&row)).or_default().push(row);
    }
    grouped
}

fn reminder_text(
    conn: &mut Connection,
    event: &Event,
    rem: &store::Reminder,
    users: &HashMap<i64, store::User>,
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
        let owned;
        let user = if let Some(user) = users.get(&signup.user_id) {
            user
        } else {
            owned = must_user(conn, signup.user_id)?;
            &owned
        };
        if !user.reminders_enabled {
            continue;
        }
        if let Some(discord_id) = &user.discord_id {
            mentions.push(format!("<@{discord_id}>"));
        } else {
            mentions.push(user.name.clone());
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
        let events = schema::events::table
            .filter(schema::events::guild_id.eq(guild_id))
            .select(Event::as_select())
            .load(tx)?;
        for event in events {
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::starts_at.eq(moved(&event.starts_at, delta)?),
                    schema::events::post_at.eq(moved_opt(event.post_at.clone(), delta)?),
                    schema::events::posted_at.eq(moved_opt(event.posted_at.clone(), delta)?),
                    schema::events::archived_at.eq(moved_opt(event.archived_at.clone(), delta)?),
                    schema::events::cutoff_at.eq(moved_opt(event.cutoff_at.clone(), delta)?),
                    schema::events::created_at.eq(moved(&event.created_at, delta)?),
                    schema::events::updated_at.eq(moved(&event.updated_at, delta)?),
                    schema::events::calendar_synced_at
                        .eq(moved_opt(event.calendar_synced_at.clone(), delta)?),
                ))
                .execute(tx)?;
            let reminders = schema::reminders::table
                .filter(schema::reminders::event_id.eq(event.id))
                .select(store::Reminder::as_select())
                .load(tx)?;
            for rem in reminders {
                if let Some(sent) = rem.sent_at {
                    diesel::update(
                        schema::reminders::table.filter(schema::reminders::id.eq(rem.id)),
                    )
                    .set(schema::reminders::sent_at.eq(moved(&sent, delta)?))
                    .execute(tx)?;
                }
            }
            let signups = schema::signups::table
                .filter(schema::signups::event_id.eq(event.id))
                .select(store::Signup::as_select())
                .load(tx)?;
            for signup in signups {
                diesel::update(schema::signups::table.filter(schema::signups::id.eq(signup.id)))
                    .set(schema::signups::created_at.eq(moved(&signup.created_at, delta)?))
                    .execute(tx)?;
            }
            let messages = schema::thread_messages::table
                .filter(schema::thread_messages::event_id.eq(event.id))
                .select(store::ThreadMessage::as_select())
                .load(tx)?;
            for message in messages {
                diesel::update(
                    schema::thread_messages::table
                        .filter(schema::thread_messages::id.eq(message.id)),
                )
                .set(schema::thread_messages::created_at.eq(moved(&message.created_at, delta)?))
                .execute(tx)?;
            }
        }
        let series_rows = schema::series::table
            .filter(schema::series::guild_id.eq(guild_id))
            .select(store::Series::as_select())
            .load(tx)?;
        for series in series_rows {
            diesel::update(schema::series::table.filter(schema::series::id.eq(series.id)))
                .set((
                    schema::series::anchor_start.eq(moved(&series.anchor_start, delta)?),
                    schema::series::end_on.eq(moved_opt(series.end_on.clone(), delta)?),
                    schema::series::created_at.eq(moved(&series.created_at, delta)?),
                ))
                .execute(tx)?;
        }
        let runs: Vec<(i64, String, String, String)> = schema::routine_runs::table
            .inner_join(schema::routines::table)
            .filter(schema::routines::guild_id.eq(guild_id))
            .select((
                schema::routine_runs::id,
                schema::routine_runs::opens_at,
                schema::routine_runs::locks_at,
                schema::routine_runs::created_at,
            ))
            .load(tx)?;
        for (id, opens, locks, created) in runs {
            diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(id)))
                .set((
                    schema::routine_runs::opens_at.eq(moved(&opens, delta)?),
                    schema::routine_runs::locks_at.eq(moved(&locks, delta)?),
                    schema::routine_runs::created_at.eq(moved(&created, delta)?),
                ))
                .execute(tx)?;
        }
        let activity = schema::activity::table
            .filter(schema::activity::guild_id.eq(guild_id))
            .order(schema::activity::id.desc())
            .limit(100_000)
            .select(store::Activity::as_select())
            .load(tx)?;
        for item in activity {
            diesel::update(schema::activity::table.filter(schema::activity::id.eq(item.id)))
                .set(schema::activity::created_at.eq(moved(&item.created_at, delta)?))
                .execute(tx)?;
        }
        let commands = schema::command_log::table
            .filter(schema::command_log::guild_id.eq(guild_id))
            .order(schema::command_log::id.desc())
            .limit(200)
            .select(store::CommandRow::as_select())
            .load(tx)?;
        for row in commands {
            diesel::update(schema::command_log::table.filter(schema::command_log::id.eq(row.id)))
                .set(schema::command_log::created_at.eq(moved(&row.created_at, delta)?))
                .execute(tx)?;
        }
        let channels = schema::channels::table
            .filter(schema::channels::guild_id.eq(guild_id))
            .select(store::Channel::as_select())
            .load(tx)?;
        for channel in channels {
            if let Some(purged) = channel.purged_at {
                diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel.id)))
                    .set(schema::channels::purged_at.eq(moved(&purged, delta)?))
                    .execute(tx)?;
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
