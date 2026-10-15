use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::access::can_create;
use super::db::{log, must_guild, must_user};
use super::model::{OptionInput, ReminderInput};
use super::series::mark_posted;
use super::template::{self, Spawn};
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::store;

#[derive(Debug, Clone)]
pub struct RoutineEffect {
    pub notice: String,
    pub jobs: Vec<DiscordJob>,
}

#[derive(Debug, Clone)]
pub struct PollButton {
    pub custom_id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct RoutinePost {
    pub channel_discord_id: String,
    pub message_id: Option<String>,
    pub title: String,
    pub body: String,
    pub color: u32,
    pub buttons: Vec<PollButton>,
}

#[derive(Debug, Clone)]
struct Routine {
    id: i64,
    public_id: String,
    guild_id: i64,
    template_id: i64,
    channel_id: i64,
    title: String,
    prompt: String,
    description: String,
    duration_minutes: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: Vec<u8>,
    active: bool,
    created_by: i64,
}

#[derive(Debug, Clone)]
struct Run {
    id: i64,
    routine_id: i64,
    opens_at: String,
    locks_at: String,
    status: String,
    days: Vec<u8>,
    winner_weekday: Option<u8>,
    event_id: Option<i64>,
    discord_message_id: Option<String>,
}

#[derive(Serialize)]
pub struct DayCheck {
    pub n: u8,
    pub name: String,
    pub on: bool,
    pub votes: i64,
}

pub fn create_routine(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    channel_id: i64,
    title: &str,
    prompt: &str,
    description: &str,
    duration_minutes: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: &[u8],
    now: DateTime<Utc>,
) -> Result<(i64, String, Vec<DiscordJob>), OpError> {
    let title = clean_title(title)?;
    let prompt = clean_prompt(prompt)?;
    let description = clean_description(description)?;
    let days = clean_days(days)?;
    if !(15..=24 * 60).contains(&duration_minutes) {
        return Err(OpError::message(
            "The event needs to last between 15 minutes and 24 hours.",
        ));
    }
    check_clock(poll_weekday, poll_minutes)?;
    check_clock(lock_weekday, lock_minutes)?;
    check_clock(0, start_minutes)?;
    store::with_tx(conn, |tx| {
        let actor = must_user(tx, actor_id)?;
        if actor.guild_id != guild_id {
            return Err(OpError::message("That person isn't in this server."));
        }
        let settings = store::settings(tx, guild_id)?;
        let roles = store::user_role_ids(tx, actor.id)?;
        if !can_create(&actor, &settings, &roles) {
            return Err(OpError::message("You can't create events here."));
        }
        let channel = store::channel(tx, channel_id)?.ok_or(OpError::NotFound)?;
        if channel.guild_id != guild_id {
            return Err(OpError::NotFound);
        }
        let stamp = clock::stamp(now);
        let public_id = crate::ids::routine_id();
        let template_id = template::insert(
            tx,
            &template::EventTemplate {
                guild_id,
                channel_id,
                title: title.clone(),
                description: description.clone(),
                duration_minutes,
                color: "#1e3a6e".into(),
                image_url: None,
                thread_enabled: settings.event_threads,
                multiple_signups: false,
                mention_everyone: false,
                attendee_role_id: None,
                cutoff_offset_minutes: None,
                options: default_options(),
                reminders: default_reminder(),
                mention_role_ids: Vec::new(),
                restrict_role_ids: Vec::new(),
            },
        )?;
        tx.execute(
            "INSERT INTO routines (
                guild_id, template_id, prompt, start_minutes,
                poll_weekday, poll_minutes, lock_weekday, lock_minutes, days_json,
                active, created_by, created_at, public_id
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
            params![
                guild_id,
                template_id,
                prompt,
                start_minutes,
                poll_weekday,
                poll_minutes,
                lock_weekday,
                lock_minutes,
                serde_json::to_string(&days).unwrap_or_else(|_| "[]".into()),
                actor_id,
                stamp,
                public_id,
            ],
        )?;
        let id = tx.last_insert_rowid();
        log(tx, guild_id, None, &format!("Saved the {title} routine."))?;
        let jobs = progress_one(tx, id, now)?;
        Ok((id, public_id, jobs))
    })
}

pub fn routine_in_guild(conn: &Connection, guild_id: i64, public_id: &str) -> Result<i64, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Err(OpError::NotFound);
    };
    conn.query_row(
        "SELECT id FROM routines WHERE guild_id = ? AND public_id = ?",
        params![guild_id, public_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(OpError::NotFound)
}

pub fn save_routine(
    conn: &mut Connection,
    routine_id: i64,
    actor_id: i64,
    channel_id: i64,
    title: &str,
    prompt: &str,
    description: &str,
    duration_minutes: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: &[u8],
    now: DateTime<Utc>,
) -> Result<RoutineEffect, OpError> {
    let title = clean_title(title)?;
    let prompt = clean_prompt(prompt)?;
    let description = clean_description(description)?;
    let days = clean_days(days)?;
    if !(15..=24 * 60).contains(&duration_minutes) {
        return Err(OpError::message(
            "The event needs to last between 15 minutes and 24 hours.",
        ));
    }
    check_clock(poll_weekday, poll_minutes)?;
    check_clock(lock_weekday, lock_minutes)?;
    check_clock(0, start_minutes)?;
    store::with_tx(conn, |tx| {
        let routine = must_routine(tx, routine_id)?;
        require_owner(tx, &routine, actor_id)?;
        let channel = store::channel(tx, channel_id)?.ok_or(OpError::NotFound)?;
        if channel.guild_id != routine.guild_id {
            return Err(OpError::NotFound);
        }
        let previous_channel = routine.channel_id;
        template::update_wording(
            tx,
            routine.template_id,
            channel_id,
            &title,
            &description,
            duration_minutes,
        )?;
        tx.execute(
            "UPDATE routines SET
                prompt = ?, start_minutes = ?, poll_weekday = ?, poll_minutes = ?,
                lock_weekday = ?, lock_minutes = ?, days_json = ?
             WHERE id = ?",
            params![
                prompt,
                start_minutes,
                poll_weekday,
                poll_minutes,
                lock_weekday,
                lock_minutes,
                serde_json::to_string(&days).unwrap_or_else(|_| "[]".into()),
                routine.id,
            ],
        )?;
        let routine = must_routine(tx, routine.id)?;
        let jobs = align_schedule(tx, &routine, previous_channel, now)?;
        log(
            tx,
            routine.guild_id,
            None,
            &format!("Saved the {} routine.", routine.title),
        )?;
        Ok(RoutineEffect {
            notice: "Saved the routine.".into(),
            jobs,
        })
    })
}

pub fn save_usual_days(
    conn: &mut Connection,
    routine_id: i64,
    actor_id: i64,
    days: &[u8],
) -> Result<String, OpError> {
    let days = clean_days(days)?;
    store::with_tx(conn, |tx| {
        let routine = must_routine(tx, routine_id)?;
        require_owner(tx, &routine, actor_id)?;
        tx.execute(
            "UPDATE routines SET days_json = ? WHERE id = ?",
            params![
                serde_json::to_string(&days).unwrap_or_else(|_| "[]".into()),
                routine.id
            ],
        )?;
        Ok("Later weeks will offer these days.".into())
    })
}

pub fn save_run_days(
    conn: &mut Connection,
    routine_id: i64,
    actor_id: i64,
    days: &[u8],
) -> Result<RoutineEffect, OpError> {
    let days = clean_days(days)?;
    store::with_tx(conn, |tx| {
        let routine = must_routine(tx, routine_id)?;
        require_owner(tx, &routine, actor_id)?;
        let Some(run) = editable_run(tx, routine.id)? else {
            return Err(OpError::message("There isn't a poll to change yet."));
        };
        tx.execute(
            "UPDATE routine_runs SET days_json = ? WHERE id = ?",
            params![
                serde_json::to_string(&days).unwrap_or_else(|_| "[]".into()),
                run.id
            ],
        )?;
        let allowed: Vec<String> = days.iter().map(|day| day.to_string()).collect();
        let marks = allowed.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!("DELETE FROM routine_votes WHERE run_id = ? AND choice NOT IN ({marks})");
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&run.id];
        for choice in &allowed {
            values.push(choice);
        }
        tx.execute(&sql, values.as_slice())?;
        let jobs = if run.status == "open" {
            vec![DiscordJob::routine(run.id)]
        } else {
            Vec::new()
        };
        Ok(RoutineEffect {
            notice: "Saved the days for this poll.".into(),
            jobs,
        })
    })
}

pub fn cast_run(
    conn: &mut Connection,
    run_id: i64,
    user_id: i64,
    day: u8,
) -> Result<RoutineEffect, OpError> {
    store::with_tx(conn, |tx| {
        let run = must_run(tx, run_id)?;
        if run.status != "open" {
            return Err(OpError::message("That poll is closed."));
        }
        if !run.days.contains(&day) {
            return Err(OpError::message("That day isn't on this poll."));
        }
        let user = must_user(tx, user_id)?;
        let routine = must_routine(tx, run.routine_id)?;
        if user.guild_id != routine.guild_id {
            return Err(OpError::message("That person isn't in this server."));
        }
        let choice = day.to_string();
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM routine_votes WHERE run_id = ? AND user_id = ? AND choice = ?",
                params![run.id, user_id, choice],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            tx.execute(
                "DELETE FROM routine_votes WHERE run_id = ? AND user_id = ? AND choice = ?",
                params![run.id, user_id, choice],
            )?;
        } else {
            tx.execute(
                "INSERT INTO routine_votes (run_id, choice, user_id, created_at) VALUES (?, ?, ?, ?)",
                params![run.id, choice, user_id, store::now_stamp()],
            )?;
        }
        let mine = selected_days(tx, run.id, user_id)?;
        let notice = if mine.is_empty() {
            "Day vote cleared.".into()
        } else {
            let names = mine
                .iter()
                .map(|day| clock::weekday_name(*day))
                .collect::<Vec<_>>()
                .join(" and ");
            format!("You can make {names}.")
        };
        Ok(RoutineEffect {
            notice,
            jobs: vec![DiscordJob::routine(run.id)],
        })
    })
}

pub fn stop_routine(
    conn: &mut Connection,
    routine_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let routine = must_routine(tx, routine_id)?;
        require_owner(tx, &routine, actor_id)?;
        tx.execute("UPDATE routines SET active = 0 WHERE id = ?", [routine.id])?;
        tx.execute(
            "DELETE FROM routine_runs WHERE routine_id = ? AND status = 'scheduled'",
            [routine.id],
        )?;
        Ok(format!(
            "Stopped {}. An open poll still locks on schedule.",
            routine.title
        ))
    })
}

pub fn maintain(
    conn: &Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let routines = routines_for_guild(conn, guild_id)?;
    if routines.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_routine: HashMap<i64, Vec<Run>> = HashMap::new();
    for run in runs_for_guild(conn, guild_id)? {
        by_routine.entry(run.routine_id).or_default().push(run);
    }
    let tz = if routines.iter().any(|routine| routine.active) {
        Some(clock::zone(&must_guild(conn, guild_id)?.timezone)?)
    } else {
        None
    };
    let mut jobs = Vec::new();
    for routine in &routines {
        let runs = by_routine.remove(&routine.id).unwrap_or_default();
        jobs.extend(advance(conn, routine, runs, tz, now)?);
    }
    Ok(jobs)
}

pub fn routine_post(conn: &Connection, run_id: i64) -> Result<Option<RoutinePost>, OpError> {
    let run = must_run(conn, run_id)?;
    let routine = must_routine(conn, run.routine_id)?;
    let Some(channel) = store::channel(conn, routine.channel_id)? else {
        return Ok(None);
    };
    let Some(discord_id) = channel.discord_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let guild = must_guild(conn, routine.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, routine.guild_id)?;
    let counts = vote_counts(conn, run.id)?;
    let body = if run.status == "locked" {
        let day = run.winner_weekday.unwrap_or(run.days[0]);
        let when = event_start(
            clock::parse_stamp(&run.locks_at)?,
            day,
            routine.start_minutes,
            tz,
        );
        format!(
            "Locked. {} won.\n\n{} is {}.",
            clock::weekday_name(day),
            routine.title,
            clock::format_when(when, tz, settings.hour_24)
        )
    } else {
        let lines = run
            .days
            .iter()
            .map(|day| {
                format!(
                    "{} — {}",
                    clock::weekday_name(*day),
                    counts.get(day).copied().unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let locks = clock::format_when(clock::parse_stamp(&run.locks_at)?, tz, settings.hour_24);
        format!(
            "{lines}\n\nLocks {locks}.\nPick every day you can make. Click a day again to take it off."
        )
    };
    let buttons = if run.status == "open" {
        run.days
            .iter()
            .map(|day| PollButton {
                custom_id: format!("marian:w:{}:{day}", run.id),
                label: clock::weekday_name(*day).to_string(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Some(RoutinePost {
        channel_discord_id: discord_id,
        message_id: run.discord_message_id,
        title: routine.prompt,
        body,
        color: 0x1e3a_6e,
        buttons,
    }))
}

pub fn remember_routine_message(
    conn: &Connection,
    run_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE routine_runs SET discord_message_id = ? WHERE id = ?",
        params![message_id, run_id],
    )?;
    Ok(())
}

pub fn routine_page_data(
    conn: &Connection,
    routine_id: i64,
) -> Result<Option<RoutineView>, OpError> {
    let Some(routine) = load_routine(conn, routine_id)? else {
        return Ok(None);
    };
    let guild = must_guild(conn, routine.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, routine.guild_id)?;
    let channel = store::channel(conn, routine.channel_id)?;
    let run = editable_run(conn, routine.id)?;
    let counts = match &run {
        Some(run) if run.status == "open" => vote_counts(conn, run.id)?,
        _ => HashMap::new(),
    };
    let days_for = |selected: &[u8]| -> Vec<DayCheck> {
        (0..7)
            .map(|n| DayCheck {
                n,
                name: clock::weekday_name(n).to_string(),
                on: selected.contains(&n),
                votes: counts.get(&n).copied().unwrap_or(0),
            })
            .collect()
    };
    let history = recent_runs(conn, routine.id, tz, settings.hour_24)?;
    Ok(Some(RoutineView {
        id: routine.public_id.clone(),
        title: routine.title.clone(),
        prompt: routine.prompt.clone(),
        description: routine.description.clone(),
        active: routine.active,
        channel: channel.map(|channel| channel.name).unwrap_or_default(),
        channel_id: routine.channel_id,
        poll_weekday: routine.poll_weekday.to_string(),
        poll_time: hhmm(routine.poll_minutes),
        lock_weekday: routine.lock_weekday.to_string(),
        lock_time: hhmm(routine.lock_minutes),
        start_time: hhmm(routine.start_minutes),
        poll_when: format!(
            "{} at {}",
            clock::weekday_name(routine.poll_weekday),
            clock_label(routine.poll_minutes, settings.hour_24, tz)
        ),
        lock_when: format!(
            "{} at {}",
            clock::weekday_name(routine.lock_weekday),
            clock_label(routine.lock_minutes, settings.hour_24, tz)
        ),
        event_when: clock_label(routine.start_minutes, settings.hour_24, tz),
        duration_minutes: routine.duration_minutes,
        usual: days_for(&routine.days),
        run: run.map(|run| RunView {
            id: run.id,
            open: run.status == "open",
            status: run.status,
            days: days_for(&run.days),
        }),
        history,
    }))
}

#[derive(Serialize)]
pub struct RoutineView {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub description: String,
    pub active: bool,
    pub channel: String,
    pub channel_id: i64,
    pub poll_weekday: String,
    pub poll_time: String,
    pub lock_weekday: String,
    pub lock_time: String,
    pub start_time: String,
    pub poll_when: String,
    pub lock_when: String,
    pub event_when: String,
    pub duration_minutes: i64,
    pub usual: Vec<DayCheck>,
    pub run: Option<RunView>,
    pub history: Vec<RunHistory>,
}

#[derive(Serialize)]
pub struct RunView {
    pub id: i64,
    pub open: bool,
    pub status: String,
    pub days: Vec<DayCheck>,
}

#[derive(Serialize)]
pub struct RunHistory {
    pub when: String,
    pub winner: String,
    pub event_id: String,
}

pub fn list_routines(conn: &Connection, guild_id: i64) -> Result<Vec<RoutineSummary>, OpError> {
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, guild_id)?;
    let mut stmt = conn.prepare(
        "SELECT r.id FROM routines r
         JOIN event_templates t ON t.id = r.template_id
         WHERE r.guild_id = ? ORDER BY r.active DESC, t.title COLLATE NOCASE",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([guild_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = Vec::new();
    for id in ids {
        let routine = must_routine(conn, id)?;
        let channel = store::channel(conn, routine.channel_id)?;
        let run = editable_run(conn, routine.id)?;
        let next = match &run {
            Some(run) => {
                let stamp = if run.status == "open" {
                    &run.locks_at
                } else {
                    &run.opens_at
                };
                let label = if run.status == "open" {
                    "Locks"
                } else {
                    "Opens"
                };
                format!(
                    "{label} {}",
                    clock::format_when(clock::parse_stamp(stamp)?, tz, settings.hour_24)
                )
            }
            None if routine.active => "Waiting for the next poll.".into(),
            None => "Stopped.".into(),
        };
        rows.push(RoutineSummary {
            id: routine.public_id,
            title: routine.title,
            channel: channel.map(|channel| channel.name).unwrap_or_default(),
            active: routine.active,
            next,
        });
    }
    Ok(rows)
}

#[derive(Serialize)]
pub struct RoutineSummary {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub active: bool,
    pub next: String,
}

pub fn count_active(conn: &Connection, guild_id: i64) -> Result<i64, OpError> {
    conn.query_row(
        "SELECT count(*) FROM routines WHERE guild_id = ? AND active = 1",
        [guild_id],
        |row| row.get(0),
    )
    .map_err(OpError::from)
}

fn align_schedule(
    conn: &Connection,
    routine: &Routine,
    previous_channel: i64,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let mut jobs = Vec::new();
    if let Some(run) = editable_run(conn, routine.id)? {
        let guild = must_guild(conn, routine.guild_id)?;
        let tz = clock::zone(&guild.timezone)?;
        if run.status == "open" {
            let locks = lock_after(
                clock::parse_stamp(&run.opens_at)?,
                tz,
                routine.lock_weekday,
                routine.lock_minutes,
            );
            let locks_stamp = clock::stamp(locks);
            if locks_stamp != run.locks_at {
                conn.execute(
                    "UPDATE routine_runs SET locks_at = ? WHERE id = ?",
                    params![locks_stamp, run.id],
                )?;
            }
            if routine.channel_id != previous_channel {
                conn.execute(
                    "UPDATE routine_runs SET discord_message_id = NULL WHERE id = ?",
                    [run.id],
                )?;
            }
            jobs.push(DiscordJob::routine(run.id));
        } else if run.status == "scheduled" {
            let (opens, locks) = window(
                now,
                tz,
                routine.poll_weekday,
                routine.poll_minutes,
                routine.lock_weekday,
                routine.lock_minutes,
            );
            let opens_stamp = clock::stamp(opens);
            let locks_stamp = clock::stamp(locks);
            if run.opens_at != opens_stamp {
                conn.execute("DELETE FROM routine_runs WHERE id = ?", [run.id])?;
            } else if run.locks_at != locks_stamp {
                conn.execute(
                    "UPDATE routine_runs SET locks_at = ? WHERE id = ?",
                    params![locks_stamp, run.id],
                )?;
            }
        }
    }
    jobs.extend(progress_one(conn, routine.id, now)?);
    Ok(jobs)
}

fn progress_one(
    conn: &Connection,
    routine_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let routine = must_routine(conn, routine_id)?;
    let runs = runs_for(conn, routine.id)?;
    let tz = if routine.active {
        Some(clock::zone(&must_guild(conn, routine.guild_id)?.timezone)?)
    } else {
        None
    };
    advance(conn, &routine, runs, tz, now)
}

fn advance(
    conn: &Connection,
    routine: &Routine,
    mut runs: Vec<Run>,
    tz: Option<Tz>,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let mut jobs = Vec::new();
    for run in &mut runs {
        if run.status == "open" && clock::parse_stamp(&run.locks_at)? <= now {
            jobs.extend(lock_run(conn, routine, run, now)?);
            run.status = "locked".to_string();
        } else if run.status == "scheduled" && clock::parse_stamp(&run.opens_at)? <= now {
            if clock::parse_stamp(&run.locks_at)? <= now {
                jobs.extend(lock_run(conn, routine, run, now)?);
                run.status = "locked".to_string();
            } else {
                conn.execute(
                    "UPDATE routine_runs SET status = 'open' WHERE id = ?",
                    [run.id],
                )?;
                run.status = "open".to_string();
                log(
                    conn,
                    routine.guild_id,
                    None,
                    &format!("The poll for {} is open.", routine.title),
                )?;
                jobs.push(DiscordJob::routine(run.id));
            }
        }
    }
    if routine.active {
        let tz = tz.ok_or(OpError::NotFound)?;
        let (opens, locks) = window(
            now,
            tz,
            routine.poll_weekday,
            routine.poll_minutes,
            routine.lock_weekday,
            routine.lock_minutes,
        );
        let opens_stamp = clock::stamp(opens);
        let already = runs
            .iter()
            .any(|run| run.opens_at == opens_stamp && run.status != "locked");
        if !already && locks > now {
            let days_json = serde_json::to_string(&routine.days).unwrap_or_else(|_| "[]".into());
            let status = if opens <= now { "open" } else { "scheduled" };
            conn.execute(
                "INSERT INTO routine_runs (routine_id, opens_at, locks_at, status, days_json, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    routine.id,
                    opens_stamp,
                    clock::stamp(locks),
                    status,
                    days_json,
                    clock::stamp(now)
                ],
            )?;
            let id = conn.last_insert_rowid();
            if status == "open" {
                log(
                    conn,
                    routine.guild_id,
                    None,
                    &format!("The poll for {} is open.", routine.title),
                )?;
                jobs.push(DiscordJob::routine(id));
            }
        }
    }
    Ok(jobs)
}

fn lock_run(
    conn: &Connection,
    routine: &Routine,
    run: &Run,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let counts = vote_counts(conn, run.id)?;
    let winner = run
        .days
        .iter()
        .copied()
        .max_by_key(|day| (counts.get(day).copied().unwrap_or(0), -i16::from(*day)))
        .unwrap_or(run.days[0]);
    let guild = must_guild(conn, routine.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let start = event_start(
        clock::parse_stamp(&run.locks_at)?,
        winner,
        routine.start_minutes,
        tz,
    );
    let event_id = insert_event(conn, routine, start, tz, now)?;
    conn.execute(
        "UPDATE routine_runs SET status = 'locked', winner_weekday = ?, event_id = ? WHERE id = ?",
        params![winner, event_id, run.id],
    )?;
    log(
        conn,
        routine.guild_id,
        Some(event_id),
        &format!(
            "Locked {} on {}.",
            routine.title,
            clock::weekday_name(winner)
        ),
    )?;
    Ok(vec![
        DiscordJob::routine(run.id),
        DiscordJob::sync(event_id),
        DiscordJob::attend(event_id),
    ])
}

fn insert_event(
    conn: &Connection,
    routine: &Routine,
    start: DateTime<Utc>,
    tz: Tz,
    now: DateTime<Utc>,
) -> Result<i64, OpError> {
    let template = template::load(conn, routine.template_id)?;
    let id = template::spawn(
        conn,
        &template,
        &Spawn {
            created_by: routine.created_by,
            starts_at: start,
            post_at: Some(start),
            status: "scheduled",
            series_id: None,
            occurrence_index: None,
            tz,
        },
    )?;
    mark_posted(conn, id, now)?;
    Ok(id)
}

fn window(
    now: DateTime<Utc>,
    tz: Tz,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let upcoming = next_strict(now, tz, poll_weekday, poll_minutes);
    let previous = upcoming - Duration::days(7);
    let previous_lock = lock_after(previous, tz, lock_weekday, lock_minutes);
    if previous <= now && now < previous_lock {
        (previous, previous_lock)
    } else {
        (
            upcoming,
            lock_after(upcoming, tz, lock_weekday, lock_minutes),
        )
    }
}

fn next_strict(now: DateTime<Utc>, tz: Tz, weekday: u8, minutes: u16) -> DateTime<Utc> {
    let hour = u32::from(minutes / 60);
    let minute = u32::from(minutes % 60);
    let dt = clock::next_weekday_at(now, tz, weekday_chrono(weekday), hour, minute, 0);
    if dt > now {
        dt
    } else {
        dt + Duration::days(7)
    }
}

fn lock_after(opens: DateTime<Utc>, tz: Tz, weekday: u8, minutes: u16) -> DateTime<Utc> {
    let hour = u32::from(minutes / 60);
    let minute = u32::from(minutes % 60);
    let dt = clock::next_weekday_at(opens, tz, weekday_chrono(weekday), hour, minute, 0);
    if dt > opens {
        dt
    } else {
        dt + Duration::days(7)
    }
}

fn event_start(locks_at: DateTime<Utc>, weekday: u8, start_minutes: u16, tz: Tz) -> DateTime<Utc> {
    let hour = u32::from(start_minutes / 60);
    let minute = u32::from(start_minutes % 60);
    let target = weekday_chrono(weekday);
    let mut date = locks_at.with_timezone(&tz).date_naive();
    for _ in 0..21 {
        if date.weekday() == target {
            let start = clock::at_local(date, hour, minute, tz);
            if start > locks_at {
                return start;
            }
        }
        date = date.succ_opt().unwrap_or(date);
    }
    locks_at + Duration::days(7)
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn clock_label(minutes: u16, hour24: bool, tz: Tz) -> String {
    let date = NaiveDate::from_ymd_opt(2026, 1, 5).expect("date");
    let local = clock::at_local(date, u32::from(minutes / 60), u32::from(minutes % 60), tz);
    clock::format_clock(local.with_timezone(&tz), hour24)
}

fn weekday_chrono(day: u8) -> Weekday {
    match day {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn default_options() -> Vec<OptionInput> {
    vec![
        option("Accepted", "✅", true),
        option("Declined", "❌", false),
        option("Tentative", "❔", false),
    ]
}

fn option(name: &str, emoji: &str, reminders: bool) -> OptionInput {
    OptionInput {
        id: None,
        name: name.into(),
        emoji: emoji.into(),
        capacity: None,
        waitlist: false,
        role_id: None,
        reminders_enabled: reminders,
    }
}

fn default_reminder() -> Vec<ReminderInput> {
    vec![ReminderInput {
        offset_minutes: 15,
        message: String::new(),
    }]
}

fn clean_title(title: &str) -> Result<String, OpError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(OpError::message(
            "Give the event a title, up to 120 characters.",
        ));
    }
    Ok(title.to_string())
}

fn clean_description(description: &str) -> Result<String, OpError> {
    let description = description.trim();
    if description.chars().count() > 2000 {
        return Err(OpError::message(
            "Keep the description under 2000 characters.",
        ));
    }
    Ok(description.to_string())
}

fn clean_prompt(prompt: &str) -> Result<String, OpError> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > 200 {
        return Err(OpError::message(
            "Give the poll a question, up to 200 characters.",
        ));
    }
    Ok(prompt.to_string())
}

fn clean_days(days: &[u8]) -> Result<Vec<u8>, OpError> {
    let mut days: Vec<u8> = days.iter().copied().filter(|day| *day < 7).collect();
    days.sort_unstable();
    days.dedup();
    if days.is_empty() {
        return Err(OpError::message(
            "Pick at least one day people can vote for.",
        ));
    }
    Ok(days)
}

fn check_clock(weekday: u8, minutes: u16) -> Result<(), OpError> {
    if weekday > 6 || minutes >= 24 * 60 {
        return Err(OpError::message("Pick a weekday and a time."));
    }
    Ok(())
}

fn require_owner(conn: &Connection, routine: &Routine, actor_id: i64) -> Result<(), OpError> {
    let actor = must_user(conn, actor_id)?;
    if actor.id == routine.created_by || actor.organizer {
        return Ok(());
    }
    Err(OpError::message(
        "You can change routines you created, or any routine if you run the server.",
    ))
}

fn vote_counts(conn: &Connection, run_id: i64) -> Result<HashMap<u8, i64>, OpError> {
    let mut stmt = conn
        .prepare("SELECT choice, count(*) FROM routine_votes WHERE run_id = ? GROUP BY choice")?;
    let rows = stmt.query_map([run_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = HashMap::new();
    for row in rows {
        let (choice, count) = row?;
        if let Ok(day) = choice.parse::<u8>() {
            counts.insert(day, count);
        }
    }
    Ok(counts)
}

fn selected_days(conn: &Connection, run_id: i64, user_id: i64) -> Result<Vec<u8>, OpError> {
    let mut stmt = conn.prepare(
        "SELECT choice FROM routine_votes WHERE run_id = ? AND user_id = ? ORDER BY choice",
    )?;
    let rows = stmt.query_map(params![run_id, user_id], |row| row.get::<_, String>(0))?;
    let mut days = Vec::new();
    for row in rows {
        if let Ok(day) = row?.parse::<u8>() {
            days.push(day);
        }
    }
    days.sort_unstable();
    Ok(days)
}

fn editable_run(conn: &Connection, routine_id: i64) -> Result<Option<Run>, OpError> {
    for run in runs_for(conn, routine_id)? {
        if run.status == "open" || run.status == "scheduled" {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

fn recent_runs(
    conn: &Connection,
    routine_id: i64,
    tz: Tz,
    hour24: bool,
) -> Result<Vec<RunHistory>, OpError> {
    let mut rows = Vec::new();
    for run in runs_for(conn, routine_id)? {
        if run.status != "locked" {
            continue;
        }
        let Some(event_id) = run.event_id else {
            continue;
        };
        let Some(event) = store::event(conn, event_id)? else {
            continue;
        };
        let guild = must_guild(conn, event.guild_id)?;
        rows.push(RunHistory {
            when: clock::format_when(clock::parse_stamp(&event.starts_at)?, tz, hour24),
            winner: clock::weekday_name(run.winner_weekday.unwrap_or(0)).to_string(),
            event_id: event_path(&guild, &event.public_id),
        });
    }
    Ok(rows)
}

fn event_path(guild: &store::Guild, public_id: &str) -> String {
    match guild.discord_id.as_deref().filter(|id| !id.is_empty()) {
        Some(server) => format!("/servers/{server}/events/{public_id}"),
        None => format!("/events/{public_id}"),
    }
}

fn runs_for(conn: &Connection, routine_id: i64) -> Result<Vec<Run>, OpError> {
    let mut stmt = conn.prepare(
        "SELECT id, routine_id, opens_at, locks_at, status, days_json, winner_weekday, event_id, discord_message_id
         FROM routine_runs WHERE routine_id = ? ORDER BY opens_at",
    )?;
    let rows = stmt.query_map([routine_id], read_run)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(OpError::from)
}

fn runs_for_guild(conn: &Connection, guild_id: i64) -> Result<Vec<Run>, OpError> {
    let mut stmt = conn.prepare(
        "SELECT routine_runs.id, routine_runs.routine_id, routine_runs.opens_at, routine_runs.locks_at,
                routine_runs.status, routine_runs.days_json, routine_runs.winner_weekday,
                routine_runs.event_id, routine_runs.discord_message_id
         FROM routine_runs
         JOIN routines ON routines.id = routine_runs.routine_id
         WHERE routines.guild_id = ?
         ORDER BY routine_runs.routine_id, routine_runs.opens_at",
    )?;
    let rows = stmt.query_map([guild_id], read_run)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(OpError::from)
}

fn routines_for_guild(conn: &Connection, guild_id: i64) -> Result<Vec<Routine>, OpError> {
    let mut stmt = conn.prepare(&format!("{ROUTINE_SQL} WHERE r.guild_id = ? ORDER BY r.id"))?;
    let rows = stmt.query_map([guild_id], map_routine)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(OpError::from)
}

fn must_run(conn: &Connection, id: i64) -> Result<Run, OpError> {
    conn.query_row(
        "SELECT id, routine_id, opens_at, locks_at, status, days_json, winner_weekday, event_id, discord_message_id
         FROM routine_runs WHERE id = ?",
        [id],
        read_run,
    )
    .optional()?
    .ok_or(OpError::NotFound)
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    let raw: String = row.get(5)?;
    Ok(Run {
        id: row.get(0)?,
        routine_id: row.get(1)?,
        opens_at: row.get(2)?,
        locks_at: row.get(3)?,
        status: row.get(4)?,
        days: serde_json::from_str(&raw).unwrap_or_default(),
        winner_weekday: row.get(6)?,
        event_id: row.get(7)?,
        discord_message_id: row.get(8)?,
    })
}

fn must_routine(conn: &Connection, id: i64) -> Result<Routine, OpError> {
    load_routine(conn, id)?.ok_or(OpError::NotFound)
}

const ROUTINE_SQL: &str =
    "SELECT r.id, r.public_id, r.guild_id, r.template_id, t.channel_id, t.title, r.prompt,
        t.description, t.duration_minutes, r.start_minutes, r.poll_weekday, r.poll_minutes,
        r.lock_weekday, r.lock_minutes, r.days_json, r.active, r.created_by
 FROM routines r
 JOIN event_templates t ON t.id = r.template_id";

fn map_routine(row: &rusqlite::Row<'_>) -> rusqlite::Result<Routine> {
    let raw: String = row.get(14)?;
    Ok(Routine {
        id: row.get(0)?,
        public_id: row.get(1)?,
        guild_id: row.get(2)?,
        template_id: row.get(3)?,
        channel_id: row.get(4)?,
        title: row.get(5)?,
        prompt: row.get(6)?,
        description: row.get(7)?,
        duration_minutes: row.get(8)?,
        start_minutes: row.get(9)?,
        poll_weekday: row.get(10)?,
        poll_minutes: row.get(11)?,
        lock_weekday: row.get(12)?,
        lock_minutes: row.get(13)?,
        days: serde_json::from_str(&raw).unwrap_or_default(),
        active: row.get::<_, i64>(15)? != 0,
        created_by: row.get(16)?,
    })
}

fn load_routine(conn: &Connection, id: i64) -> Result<Option<Routine>, OpError> {
    conn.query_row(&format!("{ROUTINE_SQL} WHERE r.id = ?"), [id], map_routine)
        .optional()
        .map_err(OpError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn world() -> Connection {
        let conn = store::open_memory().unwrap();
        conn.execute_batch(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'play', 'text', 0);
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer)
              VALUES (1, 1, 'Nell', 'nell', '#112233', 1, 1),
                     (2, 1, 'June', 'june', '#112233', 1, 0);
            ",
        )
        .unwrap();
        store::insert_default_settings(&conn, 1).unwrap();
        conn
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn a_sunday_poll_locks_on_tuesday_and_posts_the_winning_day() {
        let mut conn = world();
        let (id, public_id, jobs) = create_routine(
            &mut conn,
            1,
            1,
            1,
            "Game night",
            "Which day should we play?",
            "Bring dice on {date}.",
            120,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        assert!(
            jobs.iter()
                .any(|job| matches!(job, DiscordJob::Routine { .. })),
            "{jobs:?}"
        );
        let open = editable_run(&conn, id).unwrap().unwrap();
        assert_eq!(open.status, "open");
        assert_eq!(open.days, vec![3, 4]);

        save_run_days(&mut conn, id, 1, &[4, 5]).unwrap();
        cast_run(&mut conn, open.id, 2, 4).unwrap();
        cast_run(&mut conn, open.id, 2, 5).unwrap();
        cast_run(&mut conn, open.id, 1, 4).unwrap();

        let jobs = maintain(&conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        assert!(jobs
            .iter()
            .any(|job| matches!(job, DiscordJob::Sync { .. })));
        let event_id = jobs
            .iter()
            .find_map(|job| match job {
                DiscordJob::Sync { event_id, .. } => Some(*event_id),
                _ => None,
            })
            .unwrap();
        let event = store::event(&conn, event_id).unwrap().unwrap();
        assert_eq!(event.status, "posted");
        assert_eq!(event.title, "Game night");
        assert!(event.description.starts_with("Bring dice on "));
        assert!(!event.description.contains("{date}"));
        assert_eq!(event.starts_at, "2026-10-16T19:00:00Z");
        let next = editable_run(&conn, id).unwrap().unwrap();
        assert_eq!(next.status, "scheduled");
        assert_eq!(next.days, vec![3, 4]);
        let page = crate::present::routine_detail(
            &crate::present::Page {
                conn: &conn,
                guild_id: 1,
                user_id: 1,
                notice: None,
                invite_url: "",
                nav: "routines",
                title: "Routine",
                allowed_guilds: &[1],
            },
            &public_id,
        )
        .unwrap();
        assert!(crate::ids::parse_event_id(&public_id).is_some());
        assert!(page.contains(&public_id), "{page}");
        assert!(page.contains("Days for the next poll"), "{page}");
        assert!(page.contains("Days later weeks start from"), "{page}");
        assert!(page.contains("Game night"), "{page}");
        assert!(page.contains("Bring dice on {date}."), "{page}");
        assert!(page.contains("value=\"10:00\""), "{page}");
        assert!(page.contains("value=\"18:00\""), "{page}");
        assert!(page.contains("value=\"19:00\""), "{page}");
        assert!(page.contains("Save routine"), "{page}");
        let locks = next.locks_at.clone();
        save_routine(
            &mut conn,
            id,
            1,
            1,
            "Late night",
            "Which day should we play?",
            "Bring snacks.",
            90,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            at(2026, 10, 13, 18, 0),
        )
        .unwrap();
        let next = editable_run(&conn, id).unwrap().unwrap();
        assert_eq!(next.status, "scheduled");
        assert_eq!(next.locks_at, locks);
        assert_eq!(next.days, vec![3, 4]);
        let event = store::event(&conn, event_id).unwrap().unwrap();
        assert_eq!(event.title, "Game night");
        assert!(event.description.starts_with("Bring dice on "));
        let page = crate::present::routine_detail(
            &crate::present::Page {
                conn: &conn,
                guild_id: 1,
                user_id: 1,
                notice: None,
                invite_url: "",
                nav: "routines",
                title: "Routine",
                allowed_guilds: &[1],
            },
            &public_id,
        )
        .unwrap();
        assert!(page.contains("Late night"), "{page}");
        assert!(page.contains("Bring snacks."), "{page}");
        assert!(page.contains(&public_id), "{page}");
        let list = crate::present::routines_page(&crate::present::Page {
            conn: &conn,
            guild_id: 1,
            user_id: 1,
            notice: None,
            invite_url: "",
            nav: "routines",
            title: "Routines",
            allowed_guilds: &[1],
        })
        .unwrap();
        assert!(list.contains("Late night"), "{list}");
        assert!(list.contains(&public_id), "{list}");
        assert!(list.contains("New routine"), "{list}");
    }

    #[test]
    fn a_day_already_past_on_lock_lands_the_following_week() {
        let mut conn = world();
        let (id, _, _) = create_routine(
            &mut conn,
            1,
            1,
            1,
            "Game night",
            "Which day should we play?",
            "",
            60,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[0],
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        let open = editable_run(&conn, id).unwrap().unwrap();
        cast_run(&mut conn, open.id, 1, 0).unwrap();
        let jobs = maintain(&conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        let event_id = jobs
            .iter()
            .find_map(|job| match job {
                DiscordJob::Sync { event_id, .. } => Some(*event_id),
                _ => None,
            })
            .unwrap();
        let event = store::event(&conn, event_id).unwrap().unwrap();
        assert_eq!(event.starts_at, "2026-10-19T19:00:00Z");
    }
}
