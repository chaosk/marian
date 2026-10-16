use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;
use diesel::prelude::*;
use serde::Serialize;

use super::access::can_create;
use super::db::{log, must_guild, must_user};
use super::model::{OptionInput, ReminderInput};
use super::series::mark_posted;
use super::template::{self, Spawn};
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::schema;
use crate::store::{self, Connection};

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
        let id = diesel::insert_into(schema::routines::table)
            .values((
                schema::routines::guild_id.eq(guild_id),
                schema::routines::template_id.eq(template_id),
                schema::routines::prompt.eq(&prompt),
                schema::routines::start_minutes.eq(i64::from(start_minutes)),
                schema::routines::poll_weekday.eq(i64::from(poll_weekday)),
                schema::routines::poll_minutes.eq(i64::from(poll_minutes)),
                schema::routines::lock_weekday.eq(i64::from(lock_weekday)),
                schema::routines::lock_minutes.eq(i64::from(lock_minutes)),
                schema::routines::days_json.eq(days_json(&days)),
                schema::routines::active.eq(true),
                schema::routines::created_by.eq(actor_id),
                schema::routines::created_at.eq(&stamp),
                schema::routines::public_id.eq(&public_id),
            ))
            .returning(schema::routines::id)
            .get_result(tx)?;
        log(tx, guild_id, None, &format!("Saved the {title} routine."))?;
        let jobs = progress_one(tx, id, now)?;
        Ok((id, public_id, jobs))
    })
}

pub fn routine_in_guild(
    conn: &mut Connection,
    guild_id: i64,
    public_id: &str,
) -> Result<i64, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Err(OpError::NotFound);
    };
    schema::routines::table
        .filter(schema::routines::guild_id.eq(guild_id))
        .filter(schema::routines::public_id.eq(public_id))
        .select(schema::routines::id)
        .first(conn)
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
        diesel::update(schema::routines::table.filter(schema::routines::id.eq(routine.id)))
            .set((
                schema::routines::prompt.eq(&prompt),
                schema::routines::start_minutes.eq(i64::from(start_minutes)),
                schema::routines::poll_weekday.eq(i64::from(poll_weekday)),
                schema::routines::poll_minutes.eq(i64::from(poll_minutes)),
                schema::routines::lock_weekday.eq(i64::from(lock_weekday)),
                schema::routines::lock_minutes.eq(i64::from(lock_minutes)),
                schema::routines::days_json.eq(days_json(&days)),
            ))
            .execute(tx)?;
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
        diesel::update(schema::routines::table.filter(schema::routines::id.eq(routine.id)))
            .set(schema::routines::days_json.eq(days_json(&days)))
            .execute(tx)?;
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
        diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
            .set(schema::routine_runs::days_json.eq(days_json(&days)))
            .execute(tx)?;
        let allowed: Vec<String> = days.iter().map(|day| day.to_string()).collect();
        if allowed.is_empty() {
            diesel::delete(
                schema::routine_votes::table.filter(schema::routine_votes::run_id.eq(run.id)),
            )
            .execute(tx)?;
        } else {
            diesel::delete(
                schema::routine_votes::table
                    .filter(schema::routine_votes::run_id.eq(run.id))
                    .filter(schema::routine_votes::choice.ne_all(allowed)),
            )
            .execute(tx)?;
        }
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
        let existing: Option<i64> = schema::routine_votes::table
            .filter(schema::routine_votes::run_id.eq(run.id))
            .filter(schema::routine_votes::user_id.eq(user_id))
            .filter(schema::routine_votes::choice.eq(&choice))
            .select(schema::routine_votes::id)
            .first(tx)
            .optional()?;
        if existing.is_some() {
            diesel::delete(
                schema::routine_votes::table
                    .filter(schema::routine_votes::run_id.eq(run.id))
                    .filter(schema::routine_votes::user_id.eq(user_id))
                    .filter(schema::routine_votes::choice.eq(&choice)),
            )
            .execute(tx)?;
        } else {
            diesel::insert_into(schema::routine_votes::table)
                .values((
                    schema::routine_votes::run_id.eq(run.id),
                    schema::routine_votes::choice.eq(&choice),
                    schema::routine_votes::user_id.eq(user_id),
                    schema::routine_votes::created_at.eq(store::now_stamp()),
                ))
                .execute(tx)?;
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
        diesel::update(schema::routines::table.filter(schema::routines::id.eq(routine.id)))
            .set(schema::routines::active.eq(false))
            .execute(tx)?;
        diesel::delete(
            schema::routine_runs::table
                .filter(schema::routine_runs::routine_id.eq(routine.id))
                .filter(schema::routine_runs::status.eq("scheduled")),
        )
        .execute(tx)?;
        Ok(format!(
            "Stopped {}. An open poll still locks on schedule.",
            routine.title
        ))
    })
}

pub fn maintain(
    conn: &mut Connection,
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

pub fn routine_post(conn: &mut Connection, run_id: i64) -> Result<Option<RoutinePost>, OpError> {
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
    conn: &mut Connection,
    run_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run_id)))
        .set(schema::routine_runs::discord_message_id.eq(message_id))
        .execute(conn)?;
    Ok(())
}

pub fn routine_page_data(
    conn: &mut Connection,
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

pub fn list_routines(conn: &mut Connection, guild_id: i64) -> Result<Vec<RoutineSummary>, OpError> {
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, guild_id)?;
    let ids: Vec<i64> = schema::routines::table
        .inner_join(schema::event_templates::table)
        .filter(schema::routines::guild_id.eq(guild_id))
        .order(schema::routines::active.desc())
        .then_order_by(schema::lower(schema::event_templates::title))
        .select(schema::routines::id)
        .load(conn)?;
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

pub fn count_active(conn: &mut Connection, guild_id: i64) -> Result<i64, OpError> {
    Ok(schema::routines::table
        .filter(schema::routines::guild_id.eq(guild_id))
        .filter(schema::routines::active.eq(true))
        .select(diesel::dsl::count_star())
        .first(conn)?)
}

fn align_schedule(
    conn: &mut Connection,
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
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::locks_at.eq(&locks_stamp))
                .execute(conn)?;
            }
            if routine.channel_id != previous_channel {
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::discord_message_id.eq(None::<String>))
                .execute(conn)?;
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
                diesel::delete(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .execute(conn)?;
            } else if run.locks_at != locks_stamp {
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::locks_at.eq(&locks_stamp))
                .execute(conn)?;
            }
        }
    }
    jobs.extend(progress_one(conn, routine.id, now)?);
    Ok(jobs)
}

fn progress_one(
    conn: &mut Connection,
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
    conn: &mut Connection,
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
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::status.eq("open"))
                .execute(conn)?;
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
            let days_json = days_json(&routine.days);
            let status = if opens <= now { "open" } else { "scheduled" };
            let id = diesel::insert_into(schema::routine_runs::table)
                .values((
                    schema::routine_runs::routine_id.eq(routine.id),
                    schema::routine_runs::opens_at.eq(&opens_stamp),
                    schema::routine_runs::locks_at.eq(clock::stamp(locks)),
                    schema::routine_runs::status.eq(status),
                    schema::routine_runs::days_json.eq(&days_json),
                    schema::routine_runs::created_at.eq(clock::stamp(now)),
                ))
                .returning(schema::routine_runs::id)
                .get_result(conn)?;
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
    conn: &mut Connection,
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
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
        .set((
            schema::routine_runs::status.eq("locked"),
            schema::routine_runs::winner_weekday.eq(i64::from(winner)),
            schema::routine_runs::event_id.eq(event_id),
        ))
        .execute(conn)?;
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
    conn: &mut Connection,
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

fn require_owner(conn: &mut Connection, routine: &Routine, actor_id: i64) -> Result<(), OpError> {
    let actor = must_user(conn, actor_id)?;
    if actor.id == routine.created_by || actor.organizer {
        return Ok(());
    }
    Err(OpError::message(
        "You can change routines you created, or any routine if you run the server.",
    ))
}

fn vote_counts(conn: &mut Connection, run_id: i64) -> Result<HashMap<u8, i64>, OpError> {
    let rows: Vec<(String, i64)> = schema::routine_votes::table
        .filter(schema::routine_votes::run_id.eq(run_id))
        .group_by(schema::routine_votes::choice)
        .select((schema::routine_votes::choice, diesel::dsl::count_star()))
        .load(conn)?;
    let mut counts = HashMap::new();
    for (choice, count) in rows {
        if let Ok(day) = choice.parse::<u8>() {
            counts.insert(day, count);
        }
    }
    Ok(counts)
}

fn selected_days(conn: &mut Connection, run_id: i64, user_id: i64) -> Result<Vec<u8>, OpError> {
    let rows: Vec<String> = schema::routine_votes::table
        .filter(schema::routine_votes::run_id.eq(run_id))
        .filter(schema::routine_votes::user_id.eq(user_id))
        .order(schema::routine_votes::choice.asc())
        .select(schema::routine_votes::choice)
        .load(conn)?;
    let mut days = Vec::new();
    for choice in rows {
        if let Ok(day) = choice.parse::<u8>() {
            days.push(day);
        }
    }
    days.sort_unstable();
    Ok(days)
}

fn editable_run(conn: &mut Connection, routine_id: i64) -> Result<Option<Run>, OpError> {
    for run in runs_for(conn, routine_id)? {
        if run.status == "open" || run.status == "scheduled" {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

fn recent_runs(
    conn: &mut Connection,
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

fn runs_for(conn: &mut Connection, routine_id: i64) -> Result<Vec<Run>, OpError> {
    let rows: Vec<RunRow> = schema::routine_runs::table
        .filter(schema::routine_runs::routine_id.eq(routine_id))
        .order(schema::routine_runs::opens_at.asc())
        .select(run_columns())
        .load(conn)?;
    Ok(rows.into_iter().map(run_from_row).collect())
}

fn runs_for_guild(conn: &mut Connection, guild_id: i64) -> Result<Vec<Run>, OpError> {
    let rows: Vec<RunRow> = schema::routine_runs::table
        .inner_join(schema::routines::table)
        .filter(schema::routines::guild_id.eq(guild_id))
        .order((
            schema::routine_runs::routine_id.asc(),
            schema::routine_runs::opens_at.asc(),
        ))
        .select(run_columns())
        .load(conn)?;
    Ok(rows.into_iter().map(run_from_row).collect())
}

fn routines_for_guild(conn: &mut Connection, guild_id: i64) -> Result<Vec<Routine>, OpError> {
    let rows: Vec<RoutineRow> = schema::routines::table
        .inner_join(schema::event_templates::table)
        .filter(schema::routines::guild_id.eq(guild_id))
        .order(schema::routines::id.asc())
        .select(routine_columns())
        .load(conn)?;
    Ok(rows.into_iter().map(routine_from_row).collect())
}

fn must_run(conn: &mut Connection, id: i64) -> Result<Run, OpError> {
    load_run(conn, id)?.ok_or(OpError::NotFound)
}

fn load_run(conn: &mut Connection, id: i64) -> Result<Option<Run>, OpError> {
    let row: Option<RunRow> = schema::routine_runs::table
        .filter(schema::routine_runs::id.eq(id))
        .select(run_columns())
        .first(conn)
        .optional()?;
    Ok(row.map(run_from_row))
}

type RunRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn run_from_row(row: RunRow) -> Run {
    let (
        id,
        routine_id,
        opens_at,
        locks_at,
        status,
        days_json,
        winner_weekday,
        event_id,
        discord_message_id,
    ) = row;
    Run {
        id,
        routine_id,
        opens_at,
        locks_at,
        status,
        days: serde_json::from_str(&days_json).unwrap_or_default(),
        winner_weekday: winner_weekday.map(|day| day as u8),
        event_id,
        discord_message_id,
    }
}

fn must_routine(conn: &mut Connection, id: i64) -> Result<Routine, OpError> {
    load_routine(conn, id)?.ok_or(OpError::NotFound)
}

type RoutineRow = (
    i64,
    String,
    i64,
    i64,
    i64,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    String,
    bool,
    i64,
);

fn routine_from_row(row: RoutineRow) -> Routine {
    let (
        id,
        public_id,
        guild_id,
        template_id,
        channel_id,
        title,
        prompt,
        description,
        duration_minutes,
        start_minutes,
        poll_weekday,
        poll_minutes,
        lock_weekday,
        lock_minutes,
        days_json,
        active,
        created_by,
    ) = row;
    Routine {
        id,
        public_id,
        guild_id,
        template_id,
        channel_id,
        title,
        prompt,
        description,
        duration_minutes,
        start_minutes: start_minutes as u16,
        poll_weekday: poll_weekday as u8,
        poll_minutes: poll_minutes as u16,
        lock_weekday: lock_weekday as u8,
        lock_minutes: lock_minutes as u16,
        days: serde_json::from_str(&days_json).unwrap_or_default(),
        active,
        created_by,
    }
}

fn load_routine(conn: &mut Connection, id: i64) -> Result<Option<Routine>, OpError> {
    let row: Option<RoutineRow> = schema::routines::table
        .inner_join(schema::event_templates::table)
        .filter(schema::routines::id.eq(id))
        .select(routine_columns())
        .first(conn)
        .optional()?;
    Ok(row.map(routine_from_row))
}

fn days_json(days: &[u8]) -> String {
    serde_json::to_string(days).unwrap_or_else(|_| "[]".into())
}

fn run_columns() -> (
    schema::routine_runs::id,
    schema::routine_runs::routine_id,
    schema::routine_runs::opens_at,
    schema::routine_runs::locks_at,
    schema::routine_runs::status,
    schema::routine_runs::days_json,
    schema::routine_runs::winner_weekday,
    schema::routine_runs::event_id,
    schema::routine_runs::discord_message_id,
) {
    (
        schema::routine_runs::id,
        schema::routine_runs::routine_id,
        schema::routine_runs::opens_at,
        schema::routine_runs::locks_at,
        schema::routine_runs::status,
        schema::routine_runs::days_json,
        schema::routine_runs::winner_weekday,
        schema::routine_runs::event_id,
        schema::routine_runs::discord_message_id,
    )
}

fn routine_columns() -> (
    schema::routines::id,
    schema::routines::public_id,
    schema::routines::guild_id,
    schema::routines::template_id,
    schema::event_templates::channel_id,
    schema::event_templates::title,
    schema::routines::prompt,
    schema::event_templates::description,
    schema::event_templates::duration_minutes,
    schema::routines::start_minutes,
    schema::routines::poll_weekday,
    schema::routines::poll_minutes,
    schema::routines::lock_weekday,
    schema::routines::lock_minutes,
    schema::routines::days_json,
    schema::routines::active,
    schema::routines::created_by,
) {
    (
        schema::routines::id,
        schema::routines::public_id,
        schema::routines::guild_id,
        schema::routines::template_id,
        schema::event_templates::channel_id,
        schema::event_templates::title,
        schema::routines::prompt,
        schema::event_templates::description,
        schema::event_templates::duration_minutes,
        schema::routines::start_minutes,
        schema::routines::poll_weekday,
        schema::routines::poll_minutes,
        schema::routines::lock_weekday,
        schema::routines::lock_minutes,
        schema::routines::days_json,
        schema::routines::active,
        schema::routines::created_by,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use diesel::connection::SimpleConnection;

    fn world() -> Connection {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '99');
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (1, 1, 'play', 'text', 0);
            INSERT INTO users (id, guild_id, name, handle, color, reminders_enabled, organizer)
              VALUES (1, 1, 'Nell', 'nell', '#112233', true, true),
                     (2, 1, 'June', 'june', '#112233', true, false);
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
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
        let open = editable_run(&mut conn, id).unwrap().unwrap();
        assert_eq!(open.status, "open");
        assert_eq!(open.days, vec![3, 4]);

        save_run_days(&mut conn, id, 1, &[4, 5]).unwrap();
        cast_run(&mut conn, open.id, 2, 4).unwrap();
        cast_run(&mut conn, open.id, 2, 5).unwrap();
        cast_run(&mut conn, open.id, 1, 4).unwrap();

        let jobs = maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
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
        let event = store::event(&mut conn, event_id).unwrap().unwrap();
        assert_eq!(event.status, "posted");
        assert_eq!(event.title, "Game night");
        assert!(event.description.starts_with("Bring dice on "));
        assert!(!event.description.contains("{date}"));
        assert_eq!(event.starts_at, "2026-10-16T19:00:00Z");
        let next = editable_run(&mut conn, id).unwrap().unwrap();
        assert_eq!(next.status, "scheduled");
        assert_eq!(next.days, vec![3, 4]);
        let page = crate::present::routine_detail(
            &mut crate::present::Page {
                conn: &mut conn,
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
        let next = editable_run(&mut conn, id).unwrap().unwrap();
        assert_eq!(next.status, "scheduled");
        assert_eq!(next.locks_at, locks);
        assert_eq!(next.days, vec![3, 4]);
        let event = store::event(&mut conn, event_id).unwrap().unwrap();
        assert_eq!(event.title, "Game night");
        assert!(event.description.starts_with("Bring dice on "));
        let page = crate::present::routine_detail(
            &mut crate::present::Page {
                conn: &mut conn,
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
        let list = crate::present::routines_page(&mut crate::present::Page {
            conn: &mut conn,
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
        let open = editable_run(&mut conn, id).unwrap().unwrap();
        cast_run(&mut conn, open.id, 1, 0).unwrap();
        let jobs = maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        let event_id = jobs
            .iter()
            .find_map(|job| match job {
                DiscordJob::Sync { event_id, .. } => Some(*event_id),
                _ => None,
            })
            .unwrap();
        let event = store::event(&mut conn, event_id).unwrap().unwrap();
        assert_eq!(event.starts_at, "2026-10-19T19:00:00Z");
    }
}
