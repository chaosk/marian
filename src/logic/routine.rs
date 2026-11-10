use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;
use diesel::prelude::*;
use super::access::can_create;
use super::db::{log, must_guild, must_membership, role_names};
use super::discord::mention_role_ids;
use super::event::clean_extras;
use super::model::{DiscordField, EventExtras, OptionInput, ReminderInput};
use super::series::mark_posted;
use super::template::{self, Spawn};
use uuid::Uuid;

use crate::clock;
pub use marian_types::{ApprovalView, DayCheck, PollChoice, RoutineStep, RoutineSummary, RoutineView, RunHistory, RunView};
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::schema;
use crate::store::{self, Connection};

mod steps;
pub use steps::{
    approval_post, can_decide, decide_run, default_steps, load_steps, remember_approval_message, waiting_run_id,
    ApprovalPost, Step, StepKind,
};
use steps::{clean_steps, proceed, store_steps, waiting_run};

#[derive(Debug, Clone)]
pub struct RoutineEffect {
    pub jobs: Vec<DiscordJob>,
}

#[derive(Debug, Clone)]
pub struct PollButton {
    pub custom_id: String,
    pub label: String,
    pub tone: super::model::ButtonTone,
}

#[derive(Debug, Clone)]
pub struct RoutinePost {
    pub channel_discord_id: String,
    /// The message to edit. For a locked poll, the results reply once it is up.
    pub message_id: Option<String>,
    /// The final results of a locked poll, as their own message.
    pub results: bool,
    /// What the results answer: the post the event took over, when there was one.
    pub reply_to: Option<String>,
    /// The thread of the event a locked poll became, when the event has one:
    /// the results go there instead of under the post.
    pub thread: Option<ResultsThread>,
    pub title: String,
    pub body: String,
    pub color: u32,
    pub fields: Vec<DiscordField>,
    pub buttons: Vec<PollButton>,
    /// Who the open poll mentions: the routine's own mentions. The results
    /// reply mentions no one.
    pub mention_everyone: bool,
    pub mention_roles: Vec<String>,
}

/// Where a locked poll's results go when its event has a thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultsThread {
    pub event_id: i64,
    /// The thread in Discord, once there is one.
    pub discord_id: Option<String>,
    /// What to call the thread if it has to be opened.
    pub name: String,
}

#[derive(Debug, Clone)]
struct Routine {
    id: i64,
    public_id: Uuid,
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
    created_by: Option<i64>,
}

#[derive(Debug, Clone)]
struct Run {
    id: i64,
    routine_id: i64,
    opens_at: DateTime<Utc>,
    locks_at: DateTime<Utc>,
    status: String,
    days: Vec<u8>,
    winner_weekday: Option<u8>,
    event_id: Option<i64>,
    discord_message_id: Option<String>,
    public_id: uuid::Uuid,
    results_message_id: Option<String>,
    /// The position of the routine step the run is on.
    step: i64,
    approval_message_id: Option<String>,
    decide_by: Option<DateTime<Utc>>,
    decided_by: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::routines)]
struct NewRoutine<'a> {
    guild_id: i64,
    template_id: i64,
    prompt: &'a str,
    start_minutes: i64,
    poll_weekday: i64,
    poll_minutes: i64,
    lock_weekday: i64,
    lock_minutes: i64,
    days_json: &'a [i16],
    active: bool,
    created_by: i64,
    created_at: DateTime<Utc>,
    public_id: Uuid,
}

#[derive(Insertable)]
#[diesel(table_name = schema::routine_votes)]
struct NewVote<'a> {
    run_id: i64,
    choice: &'a str,
    membership_id: i64,
    created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::routine_runs)]
struct NewRun<'a> {
    routine_id: i64,
    opens_at: DateTime<Utc>,
    locks_at: DateTime<Utc>,
    status: &'a str,
    days_json: &'a [i16],
    created_at: DateTime<Utc>,
}

pub fn create_routine(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    channel_id: i64,
    title: &str,
    description: &str,
    duration_minutes: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: &[u8],
    extras: Option<EventExtras>,
    now: DateTime<Utc>,
) -> Result<(i64, String, Vec<DiscordJob>), OpError> {
    let title = clean_title(title)?;
    let description = clean_description(description)?;
    let days = clean_days(days)?;
    if !(15..=24 * 60).contains(&duration_minutes) {
        return Err(OpError::from(crate::msg!("err-event-length")));
    }
    check_clock(poll_weekday, poll_minutes)?;
    check_clock(lock_weekday, lock_minutes)?;
    check_clock(0, start_minutes)?;
    store::with_tx(conn, |tx| {
        let actor = must_membership(tx, actor_id)?;
        if actor.guild_id != guild_id {
            return Err(OpError::from(crate::msg!("err-person-elsewhere")));
        }
        let settings = store::settings(tx, guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if !can_create(&actor, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-cant-create-events")));
        }
        let channel = store::channel(tx, channel_id)?.ok_or(OpError::NotFound)?;
        if channel.guild_id != guild_id {
            return Err(OpError::NotFound);
        }
        let stamp = now;
        let public_id = crate::ids::routine_id();
        let extras = match extras {
            Some(extras) => clean_extras(tx, guild_id, &extras)?,
            None => {
                let (options, multiple_signups) = super::library::starting_options(tx, guild_id)?;
                EventExtras {
                    color: "#1e3a6e".into(),
                    thread_enabled: settings.event_threads,
                    multiple_signups,
                    options,
                    reminders: default_reminder(),
                    ..Default::default()
                }
            }
        };
        let template_id = template::insert(tx, &with_extras(guild_id, channel_id, &title, &description, duration_minutes, extras))?;
        let prompt = format!("Next event: {title}");
        let id = diesel::insert_into(schema::routines::table)
            .values(&NewRoutine {
                guild_id,
                template_id,
                prompt: &prompt,
                start_minutes: i64::from(start_minutes),
                poll_weekday: i64::from(poll_weekday),
                poll_minutes: i64::from(poll_minutes),
                lock_weekday: i64::from(lock_weekday),
                lock_minutes: i64::from(lock_minutes),
                days_json: &days_json(&days),
                active: true,
                created_by: actor_id,
                created_at: stamp,
                public_id,
            })
            .returning(schema::routines::id)
            .get_result(tx)?;
        store_steps(tx, id, &default_steps())?;
        log(tx, guild_id, None, &format!("Saved the {title} routine."))?;
        let jobs = progress_one(tx, id, now)?;
        Ok((id, public_id.to_string(), jobs))
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
    description: &str,
    duration_minutes: i64,
    start_minutes: u16,
    poll_weekday: u8,
    poll_minutes: u16,
    lock_weekday: u8,
    lock_minutes: u16,
    days: &[u8],
    extras: Option<EventExtras>,
    now: DateTime<Utc>,
) -> Result<RoutineEffect, OpError> {
    let title = clean_title(title)?;
    let description = clean_description(description)?;
    let days = clean_days(days)?;
    if !(15..=24 * 60).contains(&duration_minutes) {
        return Err(OpError::from(crate::msg!("err-event-length")));
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
        match extras {
            Some(extras) => {
                let extras = clean_extras(tx, routine.guild_id, &extras)?;
                template::update(
                    tx,
                    routine.template_id,
                    &with_extras(routine.guild_id, channel_id, &title, &description, duration_minutes, extras),
                )?;
            }
            None => template::update_wording(
                tx,
                routine.template_id,
                channel_id,
                &title,
                &description,
                duration_minutes,
            )?,
        }
        diesel::update(schema::routines::table.filter(schema::routines::id.eq(routine.id)))
            .set((
                schema::routines::prompt.eq(format!("Next event: {title}")),
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
        Ok(RoutineEffect { jobs })
    })
}

/// Checks steps for a routine of `guild_id` before anything is saved.
pub fn check_steps(conn: &mut Connection, guild_id: i64, steps: &[Step]) -> Result<(), OpError> {
    clean_steps(conn, guild_id, steps).map(|_| ())
}

/// Replaces a routine's steps. A request already waiting keeps going.
pub fn save_steps(
    conn: &mut Connection,
    routine_id: i64,
    actor_id: i64,
    steps: &[Step],
) -> Result<RoutineEffect, OpError> {
    store::with_tx(conn, |tx| {
        let routine = must_routine(tx, routine_id)?;
        require_owner(tx, &routine, actor_id)?;
        let steps = clean_steps(tx, routine.guild_id, steps)?;
        if load_steps(tx, routine.id)? == steps {
            return Ok(RoutineEffect { jobs: Vec::new() });
        }
        store_steps(tx, routine.id, &steps)?;
        log(tx, routine.guild_id, None, &format!("Changed the steps of the {} routine.", routine.title))?;
        Ok(RoutineEffect { jobs: Vec::new() })
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
            return Err(OpError::from(crate::msg!("err-no-poll-yet")));
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
        Ok(RoutineEffect { jobs })
    })
}

pub fn cast_run(
    conn: &mut Connection,
    run_id: i64,
    membership_id: i64,
    day: u8,
) -> Result<RoutineEffect, OpError> {
    store::with_tx(conn, |tx| {
        let run = must_run(tx, run_id)?;
        if run.status != "open" {
            return Err(OpError::from(crate::msg!("err-poll-closed")));
        }
        if !run.days.contains(&day) {
            return Err(OpError::from(crate::msg!("err-poll-day-missing")));
        }
        let user = must_membership(tx, membership_id)?;
        let routine = must_routine(tx, run.routine_id)?;
        if user.guild_id != routine.guild_id {
            return Err(OpError::from(crate::msg!("err-person-elsewhere")));
        }
        let choice = day.to_string();
        let existing: Option<i64> = schema::routine_votes::table
            .filter(schema::routine_votes::run_id.eq(run.id))
            .filter(schema::routine_votes::membership_id.eq(membership_id))
            .filter(schema::routine_votes::choice.eq(&choice))
            .select(schema::routine_votes::id)
            .first(tx)
            .optional()?;
        // Taking a vote back is always allowed; casting one asks for the roles
        // the routine's events ask of signups.
        if existing.is_none() && !user.server_manager {
            let restricts = template::load(tx, routine.template_id)?.restrict_role_ids;
            if !restricts.is_empty() {
                let roles = store::membership_role_ids(tx, &user)?;
                if !restricts.iter().any(|role| roles.contains(role)) {
                    let names = role_names(tx, &restricts)?;
                    return Err(OpError::from(crate::msg!(
                        "err-poll-limited",
                        roles = crate::i18n::each(|lang| crate::i18n::list_or(lang, &names)),
                    )));
                }
            }
        }
        if existing.is_some() {
            diesel::delete(
                schema::routine_votes::table
                    .filter(schema::routine_votes::run_id.eq(run.id))
                    .filter(schema::routine_votes::membership_id.eq(membership_id))
                    .filter(schema::routine_votes::choice.eq(&choice)),
            )
            .execute(tx)?;
        } else {
            diesel::insert_into(schema::routine_votes::table)
                .values(&NewVote {
                    run_id: run.id,
                    choice: &choice,
                    membership_id,
                    created_at: Utc::now(),
                })
                .execute(tx)?;
        }
        Ok(RoutineEffect {
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

pub fn routine_post(
    conn: &mut Connection,
    run_id: i64,
    public_url: &str,
) -> Result<Option<RoutinePost>, OpError> {
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
    let lang = crate::i18n::Lang::from_code(&settings.language).unwrap_or_default();
    let names = vote_names(conn, run.id)?;
    // Every day on the poll with who voted for it, open or final.
    let fields = run
        .days
        .iter()
        .map(|day| {
            let people = names.get(day).cloned().unwrap_or_default();
            let value = if people.is_empty() {
                "-".to_string()
            } else {
                people.join("\n")
            };
            let when = event_start(run.locks_at, *day, routine.start_minutes, tz);
            let label = poll_day_label(lang, when.with_timezone(&tz).date_naive());
            DiscordField {
                name: format!("**{label}** ({})", people.len()),
                value,
                inline: true,
            }
        })
        .collect();
    let locked = run.status == "locked";
    let picked = || {
        let day = run.winner_weekday.unwrap_or(run.days[0]);
        let when = event_start(run.locks_at, day, routine.start_minutes, tz);
        (day, poll_day_label(lang, when.with_timezone(&tz).date_naive()))
    };
    let body = if locked {
        let (day, label) = picked();
        let votes = names.get(&day).map(Vec::len).unwrap_or(0);
        crate::tr!(lang, "poll-won", day = label, count = votes)
    } else if run.status == "approval" {
        crate::tr!(lang, "poll-waiting", day = picked().1)
    } else if run.status == "skipped" {
        crate::tr!(lang, "poll-skipped")
    } else if run.status == "expired" {
        crate::tr!(lang, "poll-expired", day = picked().1)
    } else if settings.localize_time {
        // Discord shows it counting down, and the full time in each member's zone on hover.
        crate::tr!(lang, "poll-locks", when = format!("<t:{}:R>", run.locks_at.timestamp()))
    } else {
        crate::tr!(
            lang,
            "poll-locks",
            when = crate::i18n::format_when(lang, run.locks_at, tz, settings.hour_24)
        )
    };
    // The results reply only reports; editing the routine is for the poll
    // until it is settled.
    let body = if matches!(run.status.as_str(), "open" | "approval") {
        with_edit_link(body, public_url, guild.discord_id.as_deref(), routine.public_id, lang)
    } else {
        body
    };
    let buttons = if run.status == "open" {
        run.days
            .iter()
            .map(|day| {
                let when = event_start(run.locks_at, *day, routine.start_minutes, tz);
                PollButton {
                    custom_id: format!("marian:w:{}:{day}", run.public_id),
                    label: poll_day_label(lang, when.with_timezone(&tz).date_naive()),
                    tone: super::model::ButtonTone::Secondary,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    // Only the open poll mentions anyone.
    let (mention_everyone, mention_roles) = if run.status != "open" {
        (false, Vec::new())
    } else {
        let template = template::load(conn, routine.template_id)?;
        (template.mention_everyone, mention_role_ids(conn, &template.mention_role_ids)?)
    };
    let thread = match run.event_id.filter(|_| locked) {
        Some(event_id) => {
            let event = super::db::must_event(conn, event_id)?;
            if event.thread_enabled {
                Some(ResultsThread {
                    event_id,
                    discord_id: store::thread_discord_id(conn, event_id)?
                        .filter(|id| !id.is_empty()),
                    name: super::discord::stored_thread_name(conn, &event)?,
                })
            } else {
                None
            }
        }
        None => None,
    };
    let (message_id, reply_to, title) = if locked {
        (
            run.results_message_id,
            run.discord_message_id,
            crate::tr!(lang, "poll-results-title", title = &routine.title),
        )
    } else {
        (run.discord_message_id, None, crate::tr!(lang, "poll-next-title", title = &routine.title))
    };
    Ok(Some(RoutinePost {
        channel_discord_id: discord_id,
        message_id,
        results: locked,
        reply_to,
        thread,
        title,
        body,
        color: 0x1e3a_6e,
        fields,
        buttons,
        mention_everyone,
        mention_roles,
    }))
}

fn with_edit_link(
    mut body: String,
    public_url: &str,
    server: Option<&str>,
    public_id: Uuid,
    lang: crate::i18n::Lang,
) -> String {
    let Some(url) = routine_url(public_url, server, public_id) else {
        return body;
    };
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("[{}]({url})", crate::tr!(lang, "post-edit")));
    body
}

/// "Thursday, Oct 15": a day as the poll offers it.
fn poll_day_label(lang: crate::i18n::Lang, date: NaiveDate) -> String {
    crate::i18n::capitalized(&crate::i18n::format_long_day(lang, date))
}

fn routine_url(public_url: &str, server: Option<&str>, public_id: Uuid) -> Option<String> {
    let base = public_url.trim().trim_end_matches('/');
    let server = server.filter(|id| !id.is_empty())?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/servers/{server}/routines/{public_id}"))
}

pub fn remember_results_message(
    conn: &mut Connection,
    run_id: i64,
    message_id: &str,
) -> Result<(), OpError> {
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run_id)))
        .set(schema::routine_runs::results_message_id.eq(message_id))
        .execute(conn)?;
    Ok(())
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
    let (counts, names) = match &run {
        Some(run) if run.status == "open" => (vote_counts(conn, run.id)?, vote_names(conn, run.id)?),
        _ => (HashMap::new(), HashMap::new()),
    };
    let days_for = |selected: &[u8]| -> Vec<DayCheck> {
        (0..7)
            .map(|n| DayCheck {
                n,
                name: clock::weekday_name(n).to_string(),
                on: selected.contains(&n),
                votes: counts.get(&n).copied().unwrap_or(0),
                date: String::new(),
                voters: Vec::new(),
            })
            .collect()
    };
    // A poll's days in the order their events would fall once it locks.
    let run_days = |run: &Run| -> Vec<DayCheck> {
        let mut days: Vec<(DateTime<Utc>, DayCheck)> = days_for(&run.days)
            .into_iter()
            .map(|mut day| {
                let start = event_start(run.locks_at, day.n, routine.start_minutes, tz);
                day.date = clock::format_day(start.with_timezone(&tz).date_naive());
                day.voters = names.get(&day.n).cloned().unwrap_or_default();
                (start, day)
            })
            .collect();
        days.sort_by_key(|(start, _)| *start);
        days.into_iter().map(|(_, day)| day).collect()
    };
    let history = recent_runs(conn, routine.id, tz, settings.hour_24)?;
    let upcoming = match current_event(conn, routine.id)? {
        Some(event) => {
            let winner = runs_for(conn, routine.id)?
                .into_iter()
                .find(|run| run.event_id == Some(event.id))
                .and_then(|run| run.winner_weekday)
                .unwrap_or(0);
            Some(RunHistory {
                when: clock::format_when(event.starts_at, tz, settings.hour_24),
                winner: clock::weekday_name(winner).to_string(),
                href: event_path(&guild, &event.public_id),
            })
        }
        None => None,
    };
    Ok(Some(RoutineView {
        id: routine.public_id.to_string(),
        title: routine.title.clone(),
        prompt: routine.prompt.clone(),
        description: routine.description.clone(),
        active: routine.active,
        channel_id: crate::ids::public::channel(conn, routine.channel_id)?,
        channel: channel.map(|channel| channel.name).unwrap_or_default(),
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
        event: {
            let extras = routine_extras(conn, routine_id)?;
            event_dto(conn, extras)?
        },
        run: run.map(|run| RunView {
            open: run.status == "open",
            days: run_days(&run),
            opens: clock::format_when(run.opens_at, tz, settings.hour_24),
            locks: clock::format_when(run.locks_at, tz, settings.hour_24),
            status: run.status,
        }),
        history,
        upcoming,
        steps: step_views(conn, routine.id)?,
        approval: approval_view(conn, &routine, tz, settings.hour_24)?,
    }))
}

/// A routine's steps with public ids, as its page shows them.
fn step_views(conn: &mut Connection, routine_id: i64) -> Result<Vec<RoutineStep>, OpError> {
    let mut views = Vec::new();
    for step in load_steps(conn, routine_id)? {
        views.push(RoutineStep {
            kind: step.kind.code().to_string(),
            channel_id: step
                .channel_id
                .map(|id| crate::ids::public::channel(conn, id))
                .transpose()?,
            role_ids: crate::ids::public::roles(conn, &step.role_ids)?,
        });
    }
    Ok(views)
}

/// The approval a closed poll waits on, for the routine's page.
fn approval_view(conn: &mut Connection, routine: &Routine, tz: Tz, hour24: bool) -> Result<Option<ApprovalView>, OpError> {
    let Some(run) = waiting_run(conn, routine.id)? else {
        return Ok(None);
    };
    let channel = load_steps(conn, routine.id)?
        .into_iter()
        .nth(run.step as usize)
        .and_then(|step| step.channel_id)
        .map(|id| store::channel(conn, id))
        .transpose()?
        .flatten()
        .map(|channel| channel.name)
        .unwrap_or_default();
    let counts = vote_counts(conn, run.id)?;
    let winner = run.winner_weekday.unwrap_or(run.days[0]);
    let now = Utc::now();
    let mut days: Vec<(DateTime<Utc>, DayCheck)> = run
        .days
        .iter()
        .map(|day| {
            let start = event_start(run.locks_at, *day, routine.start_minutes, tz);
            (
                start,
                DayCheck {
                    n: *day,
                    name: clock::weekday_name(*day).to_string(),
                    on: start > now,
                    votes: counts.get(day).copied().unwrap_or(0),
                    date: clock::format_day(start.with_timezone(&tz).date_naive()),
                    voters: Vec::new(),
                },
            )
        })
        .collect();
    days.sort_by_key(|(start, _)| *start);
    Ok(Some(ApprovalView {
        channel,
        winner,
        lapses: clock::format_when(run.decide_by.unwrap_or(run.locks_at), tz, hour24),
        days: days.into_iter().map(|(_, day)| day).collect(),
        can_decide: false,
    }))
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
        let waiting = waiting_run(conn, routine.id)?;
        let (badge, poll) = match &run {
            _ if waiting.is_some() => {
                let day = waiting.as_ref().and_then(|run| run.winner_weekday).unwrap_or(0);
                (
                    "Approval".to_string(),
                    format!("{} won · waiting for approval", clock::weekday_name(day)),
                )
            }
            Some(run) if run.status == "open" => (
                "Voting".to_string(),
                format!(
                    "Poll locks {}",
                    clock::format_when(run.locks_at, tz, settings.hour_24)
                ),
            ),
            Some(run) => (
                "Waiting".to_string(),
                format!(
                    "Poll opens {}",
                    clock::format_when(run.opens_at, tz, settings.hour_24)
                ),
            ),
            None if routine.active => ("Waiting".to_string(), "Waiting for the next poll.".into()),
            None => ("Stopped".to_string(), String::new()),
        };
        let choices = match &run {
            Some(run) if run.status == "open" => {
                let counts = vote_counts(conn, run.id)?;
                run.days
                    .iter()
                    .map(|day| {
                        let when = event_start(run.locks_at, *day, routine.start_minutes, tz);
                        PollChoice {
                            label: clock::format_day(when.with_timezone(&tz).date_naive()),
                            votes: counts.get(day).copied().unwrap_or(0),
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let event = current_event(conn, routine.id)?
            .map(|event| {
                format!(
                    "Next event · {}",
                    clock::format_when(event.starts_at, tz, settings.hour_24)
                )
            })
            .unwrap_or_default();
        let days: Vec<&str> = routine.days.iter().map(|day| clock::weekday_short(*day)).collect();
        let schedule = format!(
            "{} at {}",
            days.join(", "),
            clock_label(routine.start_minutes, settings.hour_24, tz)
        );
        rows.push(RoutineSummary {
            schedule,
            id: routine.public_id.to_string(),
            title: routine.title,
            channel: channel.map(|channel| channel.name).unwrap_or_default(),
            active: routine.active,
            badge,
            poll,
            event,
            choices,
        });
    }
    Ok(rows)
}

fn current_event(conn: &mut Connection, routine_id: i64) -> Result<Option<store::Event>, OpError> {
    let mut found = None;
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
        if matches!(event.status.as_str(), "posted" | "scheduled") {
            found = Some(event);
        }
    }
    Ok(found)
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
                run.opens_at,
                tz,
                routine.lock_weekday,
                routine.lock_minutes,
            );
            if locks != run.locks_at {
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::locks_at.eq(locks))
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
            if run.opens_at != opens {
                diesel::delete(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .execute(conn)?;
            } else if run.locks_at != locks {
                diesel::update(
                    schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)),
                )
                .set(schema::routine_runs::locks_at.eq(locks))
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
        if run.status == "approval" && run.decide_by.is_some_and(|lapses| lapses <= now) {
            jobs.extend(steps::expire(conn, routine, run)?);
            run.status = "expired".to_string();
        } else if run.status == "open" && run.locks_at <= now {
            jobs.extend(lock_run(conn, routine, run, now)?);
            run.status = must_run(conn, run.id)?.status;
        } else if run.status == "scheduled" && run.opens_at <= now {
            if run.locks_at <= now {
                jobs.extend(lock_run(conn, routine, run, now)?);
                run.status = must_run(conn, run.id)?.status;
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
        // An open poll is this week's, even when the schedule has moved since it
        // opened: its window no longer matches, but a second poll would post a
        // second event.
        let already = runs.iter().any(|run| {
            run.status == "open" || (run.opens_at == opens && run.status != "locked")
        });
        if !already && locks > now {
            let days_json = days_json(&routine.days);
            let status = if opens <= now { "open" } else { "scheduled" };
            let id = diesel::insert_into(schema::routine_runs::table)
                .values(&NewRun {
                    routine_id: routine.id,
                    opens_at: opens,
                    locks_at: locks,
                    status,
                    days_json: &days_json,
                    created_at: now,
                })
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
    // The poll is the first step; what follows decides when the event posts.
    proceed(conn, routine, run, winner, 0, now)
}

/// Posts the event on `winner`'s day, taking over the poll's message.
fn post_run(
    conn: &mut Connection,
    routine: &Routine,
    run: &Run,
    winner: u8,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let guild = must_guild(conn, routine.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let start = event_start(
        run.locks_at,
        winner,
        routine.start_minutes,
        tz,
    );
    let event_id = insert_event(conn, routine, start, now)?;
    // The event takes over the poll's post: its sync edits that message into the
    // event, and the results go in its thread, or in a reply when it has none.
    if let Some(message) = &run.discord_message_id {
        super::discord::remember_discord_message(conn, event_id, message)?;
    }
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
    // The event first, so the results land in its thread or under the finished post.
    Ok(vec![
        DiscordJob::sync(event_id),
        DiscordJob::attend(event_id),
        DiscordJob::routine(run.id),
    ])
}

fn insert_event(
    conn: &mut Connection,
    routine: &Routine,
    start: DateTime<Utc>,
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

/// A routine's template: its wording, and everything else its events carry.
fn with_extras(
    guild_id: i64,
    channel_id: i64,
    title: &str,
    description: &str,
    duration_minutes: i64,
    extras: EventExtras,
) -> template::EventTemplate {
    template::EventTemplate {
        guild_id,
        channel_id,
        title: title.to_string(),
        description: description.to_string(),
        location: extras.location,
        duration_minutes,
        color: extras.color,
        image_url: None,
        thread_enabled: extras.thread_enabled,
        multiple_signups: extras.multiple_signups,
        mention_everyone: extras.mention_everyone,
        attendee_role_id: extras.attendee_role_id,
        cutoff_offset_minutes: extras.cutoff_offset_minutes,
        options: extras.options,
        reminders: extras.reminders,
        mention_role_ids: extras.mention_role_ids,
        restrict_role_ids: extras.restrict_role_ids,
    }
}

/// The page's form of what a routine's events carry, roles by public id.
pub fn event_dto(conn: &mut Connection, extras: EventExtras) -> Result<marian_types::RoutineEvent, OpError> {
    use crate::ids::public;
    let options = extras
        .options
        .into_iter()
        .map(|opt| {
            let kind = store::library_type(conn, opt.type_id)?.ok_or(OpError::NotFound)?;
            Ok(marian_types::SignupOption {
                id: None,
                type_id: kind.public_id.to_string(),
                name: kind.name,
                emoji: kind.emoji,
                attending: kind.attending,
                capacity: opt.capacity,
                role_id: public::role(conn, opt.role_id)?,
            })
        })
        .collect::<Result<_, OpError>>()?;
    Ok(marian_types::RoutineEvent {
        location: extras.location,
        color: extras.color,
        thread_enabled: extras.thread_enabled,
        multiple_signups: extras.multiple_signups,
        mention_everyone: extras.mention_everyone,
        attendee_role_id: public::role(conn, extras.attendee_role_id)?,
        mention_role_ids: public::roles(conn, &extras.mention_role_ids)?,
        restrict_role_ids: public::roles(conn, &extras.restrict_role_ids)?,
        cutoff_minutes: extras.cutoff_offset_minutes,
        options,
        reminders: extras
            .reminders
            .into_iter()
            .map(|rem| marian_types::Reminder {
                offset_minutes: rem.offset_minutes,
                message: rem.message,
                sent: false,
            })
            .collect(),
    })
}

/// What the page sent, as the routine keeps it. A role id from another server
/// is refused.
pub fn event_from(
    conn: &mut Connection,
    guild_id: i64,
    event: &marian_types::RoutineEvent,
) -> Result<EventExtras, OpError> {
    use crate::ids::public;
    let options = event
        .options
        .iter()
        .map(|opt| {
            let kind = store::library_type_by_public(conn, guild_id, &opt.type_id)?
                .ok_or_else(|| OpError::from(crate::msg!("err-type-elsewhere")))?;
            Ok(OptionInput {
                id: None,
                type_id: kind.id,
                capacity: opt.capacity,
                role_id: public::role_id(conn, guild_id, opt.role_id.as_deref())?,
            })
        })
        .collect::<Result<_, OpError>>()?;
    Ok(EventExtras {
        location: event.location.clone(),
        color: event.color.clone(),
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        attendee_role_id: public::role_id(conn, guild_id, event.attendee_role_id.as_deref())?,
        mention_role_ids: public::role_ids(conn, guild_id, &event.mention_role_ids)?,
        restrict_role_ids: public::role_ids(conn, guild_id, &event.restrict_role_ids)?,
        cutoff_offset_minutes: event.cutoff_minutes,
        options,
        reminders: event
            .reminders
            .iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message.clone(),
            })
            .collect(),
    })
}

/// What a routine's events carry beyond their wording, as its page shows it.
pub fn routine_extras(conn: &mut Connection, routine_id: i64) -> Result<EventExtras, OpError> {
    let routine = must_routine(conn, routine_id)?;
    let template = template::load(conn, routine.template_id)?;
    Ok(EventExtras {
        location: template.location,
        color: template.color,
        thread_enabled: template.thread_enabled,
        multiple_signups: template.multiple_signups,
        mention_everyone: template.mention_everyone,
        attendee_role_id: template.attendee_role_id,
        mention_role_ids: template.mention_role_ids,
        restrict_role_ids: template.restrict_role_ids,
        cutoff_offset_minutes: template.cutoff_offset_minutes,
        options: template.options,
        reminders: template.reminders,
    })
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
        return Err(OpError::from(crate::msg!("err-title-length")));
    }
    Ok(title.to_string())
}

fn clean_description(description: &str) -> Result<String, OpError> {
    let description = description.trim();
    if description.chars().count() > 2000 {
        return Err(OpError::from(crate::msg!("err-description-length")));
    }
    Ok(description.to_string())
}

fn clean_days(days: &[u8]) -> Result<Vec<u8>, OpError> {
    let mut days: Vec<u8> = days.iter().copied().filter(|day| *day < 7).collect();
    days.sort_unstable();
    days.dedup();
    if days.is_empty() {
        return Err(OpError::from(crate::msg!("err-poll-no-days")));
    }
    Ok(days)
}

fn check_clock(weekday: u8, minutes: u16) -> Result<(), OpError> {
    if weekday > 6 || minutes >= 24 * 60 {
        return Err(OpError::from(crate::msg!("err-weekday-time")));
    }
    Ok(())
}

fn require_owner(conn: &mut Connection, routine: &Routine, actor_id: i64) -> Result<(), OpError> {
    let actor = must_membership(conn, actor_id)?;
    if routine.created_by == Some(actor.id) || actor.server_manager {
        return Ok(());
    }
    Err(OpError::from(crate::msg!("err-routine-not-yours")))
}

fn vote_names(conn: &mut Connection, run_id: i64) -> Result<HashMap<u8, Vec<String>>, OpError> {
    let rows: Vec<(String, String)> = schema::routine_votes::table
        .inner_join(schema::memberships::table)
        .filter(schema::routine_votes::run_id.eq(run_id))
        .order(schema::routine_votes::created_at.asc())
        .then_order_by(schema::routine_votes::membership_id.asc())
        .select((
            schema::routine_votes::choice,
            schema::memberships::name,
        ))
        .load(conn)?;
    let mut names: HashMap<u8, Vec<String>> = HashMap::new();
    for (choice, name) in rows {
        if let Ok(day) = choice.parse::<u8>() {
            names.entry(day).or_default().push(name);
        }
    }
    Ok(names)
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

/// A poll's row key, from the public id its Discord buttons carry.
pub fn run_by_public(conn: &mut Connection, public: uuid::Uuid) -> Result<Option<i64>, OpError> {
    Ok(schema::routine_runs::table
        .filter(schema::routine_runs::public_id.eq(public))
        .select(schema::routine_runs::id)
        .first::<i64>(conn)
        .optional()?)
}

/// This week's poll while people can vote on it.
pub fn open_run_id(conn: &mut Connection, routine_id: i64) -> Result<Option<i64>, OpError> {
    Ok(editable_run(conn, routine_id)?
        .filter(|run| run.status == "open")
        .map(|run| run.id))
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
            when: clock::format_when(event.starts_at, tz, hour24),
            winner: clock::weekday_name(run.winner_weekday.unwrap_or(0)).to_string(),
            href: event_path(&guild, &event.public_id),
        });
    }
    Ok(rows)
}

fn event_path(guild: &store::Guild, public_id: impl std::fmt::Display) -> String {
    match guild.discord_id.as_deref().filter(|id| !id.is_empty()) {
        Some(server) => format!("/servers/{server}/events/{public_id}"),
        None => format!("/events/{public_id}"),
    }
}

fn runs_for(conn: &mut Connection, routine_id: i64) -> Result<Vec<Run>, OpError> {
    let rows = store::RoutineRun::for_routine(routine_id)
        .order(schema::routine_runs::opens_at.asc())
        .load(conn)?;
    Ok(rows.into_iter().map(run_from_stored).collect())
}

fn runs_for_guild(conn: &mut Connection, guild_id: i64) -> Result<Vec<Run>, OpError> {
    let rows = schema::routine_runs::table
        .inner_join(schema::routines::table)
        .filter(schema::routines::guild_id.eq(guild_id))
        .order((
            schema::routine_runs::routine_id.asc(),
            schema::routine_runs::opens_at.asc(),
        ))
        .select(store::RoutineRun::as_select())
        .load(conn)?;
    Ok(rows.into_iter().map(run_from_stored).collect())
}

fn routines_for_guild(conn: &mut Connection, guild_id: i64) -> Result<Vec<Routine>, OpError> {
    let rows = schema::routines::table
        .inner_join(schema::event_templates::table)
        .filter(schema::routines::guild_id.eq(guild_id))
        .order(schema::routines::id.asc())
        .select((
            store::Routine::as_select(),
            store::StoredTemplate::as_select(),
        ))
        .load(conn)?;
    Ok(rows
        .into_iter()
        .map(|(routine, template)| routine_from_stored(routine, template))
        .collect())
}

fn must_run(conn: &mut Connection, id: i64) -> Result<Run, OpError> {
    load_run(conn, id)?.ok_or(OpError::NotFound)
}

fn load_run(conn: &mut Connection, id: i64) -> Result<Option<Run>, OpError> {
    let row = store::RoutineRun::by_id(id).first(conn).optional()?;
    Ok(row.map(run_from_stored))
}

fn run_from_stored(row: store::RoutineRun) -> Run {
    Run {
        id: row.id,
        routine_id: row.routine_id,
        opens_at: row.opens_at,
        locks_at: row.locks_at,
        status: row.status,
        days: weekday_numbers(row.days_json),
        winner_weekday: row.winner_weekday.map(|day| day as u8),
        event_id: row.event_id,
        discord_message_id: row.discord_message_id,
        public_id: row.public_id,
        results_message_id: row.results_message_id,
        step: row.step,
        approval_message_id: row.approval_message_id,
        decide_by: row.decide_by,
        decided_by: row.decided_by,
    }
}

fn must_routine(conn: &mut Connection, id: i64) -> Result<Routine, OpError> {
    load_routine(conn, id)?.ok_or(OpError::NotFound)
}

fn routine_from_stored(routine: store::Routine, template: store::StoredTemplate) -> Routine {
    Routine {
        id: routine.id,
        public_id: routine.public_id,
        guild_id: routine.guild_id,
        template_id: routine.template_id,
        channel_id: template.channel_id,
        title: template.title,
        prompt: routine.prompt,
        description: template.description,
        duration_minutes: template.duration_minutes,
        start_minutes: routine.start_minutes as u16,
        poll_weekday: routine.poll_weekday as u8,
        poll_minutes: routine.poll_minutes as u16,
        lock_weekday: routine.lock_weekday as u8,
        lock_minutes: routine.lock_minutes as u16,
        days: weekday_numbers(routine.days_json),
        active: routine.active,
        created_by: routine.created_by,
    }
}

fn load_routine(conn: &mut Connection, id: i64) -> Result<Option<Routine>, OpError> {
    let row = schema::routines::table
        .inner_join(schema::event_templates::table)
        .filter(schema::routines::id.eq(id))
        .select((
            store::Routine::as_select(),
            store::StoredTemplate::as_select(),
        ))
        .first(conn)
        .optional()?;
    Ok(row.map(|(routine, template)| routine_from_stored(routine, template)))
}

fn days_json(days: &[u8]) -> Vec<i16> {
    days.iter().copied().map(i16::from).collect()
}

fn weekday_numbers(days: Vec<i16>) -> Vec<u8> {
    days.into_iter().map(|day| day as u8).collect()
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
            INSERT INTO option_library (id, guild_id, name, emoji, attending) VALUES
              (301, 1, 'Playing', '🎲', true), (302, 1, 'Watching', '👀', false);
            INSERT INTO users (discord_id) VALUES ('nell'), ('june');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, true FROM users WHERE discord_id = 'nell'
            UNION ALL
            SELECT 2, id, 1, 'June', 'june', '#112233', true, false FROM users WHERE discord_id = 'june';
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
    fn the_poll_mentions_and_limits_votes_as_the_routines_events_do() {
        let mut conn = world();
        conn.batch_execute(
            "
            UPDATE channels SET discord_id = '77' WHERE id = 1;
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES
              (501, 1, 'Players', '#000000', '501501'), (502, 1, 'Pings', '#000000', '502502');
            INSERT INTO users (discord_id) VALUES ('ollie');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 3, id, 1, 'Ollie', 'ollie', '#112233', true, false FROM users WHERE discord_id = 'ollie';
            INSERT INTO membership_roles (membership_id, role_id) VALUES (3, 501);
            ",
        )
        .unwrap();
        let (id, _, _) = create_routine(
            &mut conn,
            1,
            1,
            1,
            "Game night",
            "",
            120,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            Some(EventExtras {
                color: "#2f6b4f".into(),
                options: vec![OptionInput::of_type(301)],
                mention_everyone: true,
                mention_role_ids: vec![502],
                restrict_role_ids: vec![501],
                ..Default::default()
            }),
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        let open = editable_run(&mut conn, id).unwrap().unwrap();

        let post = routine_post(&mut conn, open.id, "https://marian.test").unwrap().unwrap();
        assert!(post.mention_everyone);
        assert_eq!(post.mention_roles, vec!["502502".to_string()]);

        let refused = cast_run(&mut conn, open.id, 2, 3).unwrap_err();
        assert_eq!(refused.to_string(), "This poll is limited to Players.");
        cast_run(&mut conn, open.id, 3, 3).unwrap();
        // A server manager votes whatever their roles.
        cast_run(&mut conn, open.id, 1, 3).unwrap();
        // Losing the role still lets a vote be taken back.
        conn.batch_execute("DELETE FROM membership_roles WHERE membership_id = 3;").unwrap();
        cast_run(&mut conn, open.id, 3, 3).unwrap();
        assert_eq!(vote_counts(&mut conn, open.id).unwrap().get(&3).copied(), Some(1));
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
            "Bring dice on {date}.",
            120,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            Some(EventExtras {
                location: "The cellar".into(),
                color: "#2f6b4f".into(),
                multiple_signups: true,
                options: vec![OptionInput::of_type(301), OptionInput::of_type(302)],
                reminders: vec![ReminderInput {
                    offset_minutes: 60,
                    message: "Snacks".into(),
                }],
                ..Default::default()
            }),
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
        assert_eq!(event.description, "Bring dice on {date}.", "wording is copied as written");
        assert_eq!(event.starts_at, at(2026, 10, 16, 19, 0));
        assert_eq!(event.location, "The cellar", "the event carries the routine's details");
        assert_eq!(event.color, "#2f6b4f");
        assert!(event.multiple_signups);
        let names: Vec<String> = store::options_for(&mut conn, &event).unwrap().into_iter().map(|opt| opt.name).collect();
        assert_eq!(names, ["Playing", "Watching"]);
        let reminders = store::reminders_for(&mut conn, &event).unwrap();
        assert_eq!((reminders[0].offset_minutes, reminders[0].message.as_str()), (60, "Snacks"));
        let shown = routine_page_data(&mut conn, id).unwrap().unwrap();
        assert_eq!(shown.event.location, "The cellar");
        assert_eq!(shown.event.options.len(), 2);
        let next = editable_run(&mut conn, id).unwrap().unwrap();
        assert_eq!(next.status, "scheduled");
        assert_eq!(next.days, vec![3, 4]);
        let rows = list_routines(&mut conn, 1).unwrap();
        assert_eq!(rows[0].badge, "Waiting");
        assert_eq!(rows[0].poll, "Poll opens Sun, Oct 18 · 10:00 AM");
        assert_eq!(rows[0].event, "Next event · Fri, Oct 16 · 7:00 PM");
        assert!(rows[0].choices.is_empty());
        assert!(crate::ids::parse_event_id(&public_id).is_some());
        let shown = routine_page_data(&mut conn, id).unwrap().unwrap();
        assert_eq!(shown.id, public_id);
        assert!(!shown.run.as_ref().unwrap().open);
        let run = shown.run.as_ref().unwrap();
        assert_eq!(run.opens, "Sun, Oct 18 · 10:00 AM");
        assert_eq!(run.locks, "Tue, Oct 20 · 6:00 PM");
        let order: Vec<&str> = run.days.iter().map(|day| day.date.as_str()).collect();
        assert_eq!(
            order,
            ["Tue, Oct 20", "Wed, Oct 21", "Thu, Oct 22", "Fri, Oct 23", "Sat, Oct 24", "Sun, Oct 25", "Mon, Oct 26"],
            "days run in the order their events fall after the lock"
        );
        let upcoming = shown.upcoming.as_ref().unwrap();
        assert_eq!((upcoming.winner.as_str(), upcoming.when.as_str()), ("Friday", "Fri, Oct 16 · 7:00 PM"));
        assert_eq!(rows[0].schedule, "Thu, Fri at 7:00 PM");
        assert_eq!(shown.title, "Game night");
        assert_eq!(shown.description, "Bring dice on {date}.");
        assert_eq!(shown.poll_time, "10:00");
        assert_eq!(shown.lock_time, "18:00");
        assert_eq!(shown.start_time, "19:00");
        let locks = next.locks_at.clone();
        save_routine(
            &mut conn,
            id,
            1,
            1,
            "Late night",
            "Bring snacks.",
            90,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            None,
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
        let shown = routine_page_data(&mut conn, id).unwrap().unwrap();
        assert_eq!(shown.title, "Late night");
        assert_eq!(shown.description, "Bring snacks.");
        assert_eq!(shown.id, public_id);
        let rows = list_routines(&mut conn, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Late night");
        assert_eq!(rows[0].id, public_id);
    }

    /// A Tuesday-locking routine for Thursday or Friday whose closed poll asks
    /// the Officers in #officers before it posts.
    fn approved_routine(conn: &mut Connection) -> i64 {
        conn.batch_execute(
            "
            UPDATE channels SET discord_id = '77' WHERE id = 1;
            INSERT INTO channels (id, guild_id, name, kind, position, discord_id) VALUES (2, 1, 'officers', 'text', 1, '88');
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES (501, 1, 'Officers', '#000000', '501501');
            INSERT INTO users (discord_id) VALUES ('ollie');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 3, id, 1, 'Ollie', 'ollie', '#112233', true, false FROM users WHERE discord_id = 'ollie';
            INSERT INTO membership_roles (membership_id, role_id) VALUES (2, 501);
            ",
        )
        .unwrap();
        let (id, _, _) = create_routine(
            conn,
            1,
            1,
            1,
            "Game night",
            "",
            120,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            Some(EventExtras {
                color: "#2f6b4f".into(),
                options: vec![OptionInput::of_type(301)],
                ..Default::default()
            }),
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        let approval = Step {
            kind: StepKind::Approval,
            channel_id: Some(2),
            role_ids: vec![501],
        };
        save_steps(conn, id, 1, &[Step::of(StepKind::Poll), approval, Step::of(StepKind::Post)]).unwrap();
        id
    }

    #[test]
    fn a_closed_poll_waits_for_an_officer_to_confirm_a_day() {
        let mut conn = world();
        let id = approved_routine(&mut conn);
        let run = open_run_id(&mut conn, id).unwrap().unwrap();
        cast_run(&mut conn, run, 2, 4).unwrap();

        let jobs = maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        assert!(!jobs.iter().any(|job| matches!(job, DiscordJob::Sync { .. })), "nothing posts yet: {jobs:?}");
        let waiting = must_run(&mut conn, run).unwrap();
        assert_eq!((waiting.status.as_str(), waiting.winner_weekday, waiting.step), ("approval", Some(4), 1));
        assert_eq!(waiting.decide_by, Some(at(2026, 10, 16, 0, 0)), "lapses as Friday begins");

        let poll = routine_post(&mut conn, run, "").unwrap().unwrap();
        assert!(poll.body.contains("waiting for confirmation"), "{}", poll.body);
        assert!(poll.buttons.is_empty() && poll.mention_roles.is_empty());

        let ask = approval_post(&mut conn, run, "", at(2026, 10, 14, 9, 0)).unwrap().unwrap();
        assert_eq!(ask.channel_discord_id, "88");
        assert_eq!(ask.mention_roles, ["501501"]);
        let ids: Vec<&str> = ask.buttons.iter().map(|button| button.custom_id.rsplit(':').next().unwrap()).collect();
        assert_eq!(ids, ["3", "4", "skip"], "each day in date order, then skip");
        assert_eq!(ask.buttons[1].tone, crate::logic::ButtonTone::Primary, "the winner stands out");

        let rows = list_routines(&mut conn, 1).unwrap();
        assert_eq!((rows[0].badge.as_str(), rows[0].poll.as_str()), ("Approval", "Friday won · waiting for approval"));
        let shown = routine_page_data(&mut conn, id).unwrap().unwrap();
        let approval = shown.approval.unwrap();
        assert_eq!((approval.channel.as_str(), approval.winner), ("officers", 4));
        assert_eq!(shown.steps.iter().map(|step| step.kind.as_str()).collect::<Vec<_>>(), ["poll", "approval", "post"]);

        assert!(!can_decide(&mut conn, id, 3).unwrap());
        let refused = decide_run(&mut conn, run, 3, Some(3), at(2026, 10, 14, 9, 0)).unwrap_err();
        assert!(refused.to_string().contains("Only the roles"), "{refused}");

        assert!(can_decide(&mut conn, id, 2).unwrap());
        let effect = decide_run(&mut conn, run, 2, Some(3), at(2026, 10, 14, 9, 0)).unwrap();
        let event_id = effect
            .jobs
            .iter()
            .find_map(|job| match job {
                DiscordJob::Sync { event_id, .. } => Some(*event_id),
                _ => None,
            })
            .expect("the event posts");
        let event = store::event(&mut conn, event_id).unwrap().unwrap();
        assert_eq!(event.starts_at, at(2026, 10, 15, 19, 0), "on the day June picked, not the winner");
        let done = must_run(&mut conn, run).unwrap();
        assert_eq!((done.status.as_str(), done.winner_weekday, done.event_id), ("locked", Some(3), Some(event_id)));

        conn.batch_execute("UPDATE routine_runs SET approval_message_id = '900'").unwrap();
        let answered = approval_post(&mut conn, run, "", at(2026, 10, 14, 9, 0)).unwrap().unwrap();
        assert!(answered.body.starts_with("June confirmed **Thursday"), "{}", answered.body);
        assert!(answered.buttons.is_empty() && answered.mention_roles.is_empty());
        let again = decide_run(&mut conn, run, 1, None, at(2026, 10, 14, 9, 0)).unwrap_err();
        assert_eq!(again.to_string(), "This request has already been answered.");
    }

    #[test]
    fn an_approval_can_skip_the_week_or_lapse() {
        let mut conn = world();
        let id = approved_routine(&mut conn);
        let run = open_run_id(&mut conn, id).unwrap().unwrap();
        maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        let early = decide_run(&mut conn, run, 1, Some(9), at(2026, 10, 14, 9, 0)).unwrap_err();
        assert_eq!(early.to_string(), "That day isn't on this poll.");
        decide_run(&mut conn, run, 1, None, at(2026, 10, 14, 9, 0)).unwrap();
        assert_eq!(must_run(&mut conn, run).unwrap().status, "skipped");
        let poll = routine_post(&mut conn, run, "").unwrap().unwrap();
        assert_eq!(poll.body, "There's no event this week.");
        assert_eq!(must_run(&mut conn, run).unwrap().event_id, None, "no event was made");

        // Next week nobody answers, so the request lapses as the winning day begins.
        maintain(&mut conn, 1, at(2026, 10, 18, 10, 0)).unwrap();
        let next = open_run_id(&mut conn, id).unwrap().unwrap();
        cast_run(&mut conn, next, 2, 3).unwrap();
        maintain(&mut conn, 1, at(2026, 10, 20, 18, 0)).unwrap();
        assert_eq!(must_run(&mut conn, next).unwrap().status, "approval");
        let late = decide_run(&mut conn, next, 1, Some(3), at(2026, 10, 22, 20, 0)).unwrap_err();
        assert_eq!(late.to_string(), "That day has already started. Pick a later one.");
        let jobs = maintain(&mut conn, 1, at(2026, 10, 22, 0, 0)).unwrap();
        assert_eq!(must_run(&mut conn, next).unwrap().status, "expired");
        assert!(jobs.iter().any(|job| matches!(job, DiscordJob::Routine { run_id, .. } if *run_id == next)));
        assert!(!jobs.iter().any(|job| matches!(job, DiscordJob::Sync { .. })));
        let poll = routine_post(&mut conn, next, "").unwrap().unwrap();
        assert!(poll.body.starts_with("No one confirmed **Thursday"), "{}", poll.body);
    }

    #[test]
    fn a_routine_polls_first_and_posts_last() {
        let mut conn = world();
        let id = approved_routine(&mut conn);
        let poll = Step::of(StepKind::Poll);
        let post = Step::of(StepKind::Post);
        let ask = |channel_id| Step {
            kind: StepKind::Approval,
            channel_id,
            role_ids: vec![501, 999],
        };
        for wrong in [
            vec![post.clone(), poll.clone()],
            vec![poll.clone()],
            vec![poll.clone(), ask(Some(2)), ask(Some(2)), post.clone()],
            vec![poll.clone(), ask(None), post.clone()],
        ] {
            assert!(save_steps(&mut conn, id, 1, &wrong).is_err(), "{wrong:?}");
        }
        save_steps(&mut conn, id, 1, &[poll.clone(), ask(Some(2)), post.clone()]).unwrap();
        assert_eq!(load_steps(&mut conn, id).unwrap()[1].role_ids, [501], "a role from elsewhere is dropped");
        save_steps(&mut conn, id, 1, &default_steps()).unwrap();
        assert_eq!(load_steps(&mut conn, id).unwrap(), default_steps());
    }

    #[test]
    fn moving_the_poll_while_one_is_open_keeps_a_single_poll() {
        let mut conn = world();
        let (id, _, _) = create_routine(
            &mut conn, 1, 1, 1, "Game night", "", 120, 19 * 60, 6, 10 * 60, 1, 18 * 60, &[3, 4], None,
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        // The poll opened Sunday at 10:00. Now it opens at 9:30, then at 22:30.
        for (minutes, now) in [(9 * 60 + 30, at(2026, 10, 12, 13, 0)), (22 * 60 + 30, at(2026, 10, 12, 14, 0))] {
            save_routine(
                &mut conn, id, 1, 1, "Game night", "", 120, 19 * 60, 6, minutes, 1, 18 * 60, &[3, 4], None, now,
            )
            .unwrap();
            maintain(&mut conn, 1, now).unwrap();
        }
        let live: Vec<String> = runs_for(&mut conn, id)
            .unwrap()
            .into_iter()
            .filter(|run| run.status != "locked")
            .map(|run| run.status)
            .collect();
        assert_eq!(live, ["open"], "one poll, not one per schedule change");
        let jobs = maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        let events = jobs.iter().filter(|job| matches!(job, DiscordJob::Sync { .. })).count();
        assert_eq!(events, 1, "locking posts one event");
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
            "",
            60,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[0],
            None,
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
        assert_eq!(event.starts_at, at(2026, 10, 19, 19, 0));
    }

    #[test]
    fn a_locked_poll_hands_its_post_to_the_event_and_posts_the_results_in_its_thread() {
        let mut conn = world();
        conn.batch_execute("UPDATE channels SET discord_id = '123' WHERE id = 1")
            .unwrap();
        let (id, _, _) = create_routine(
            &mut conn, 1, 1, 1, "Game night", "", 120, 19 * 60, 6, 10 * 60, 1, 18 * 60, &[3, 4], None,
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        let open = editable_run(&mut conn, id).unwrap().unwrap();
        remember_routine_message(&mut conn, open.id, "555").unwrap();
        cast_run(&mut conn, open.id, 1, 3).unwrap();
        cast_run(&mut conn, open.id, 2, 3).unwrap();
        cast_run(&mut conn, open.id, 2, 4).unwrap();

        let jobs = maintain(&mut conn, 1, at(2026, 10, 13, 18, 0)).unwrap();
        let event_id = jobs
            .iter()
            .find_map(|job| match job {
                DiscordJob::Sync { event_id, .. } => Some(*event_id),
                _ => None,
            })
            .unwrap();
        let event = store::event(&mut conn, event_id).unwrap().unwrap();
        assert_eq!(event.discord_message_id.as_deref(), Some("555"), "the event edits the poll's post");
        let sync = jobs.iter().position(|job| matches!(job, DiscordJob::Sync { .. })).unwrap();
        let results = jobs.iter().position(|job| matches!(job, DiscordJob::Routine { .. })).unwrap();
        assert!(sync < results, "the event goes up before the results reply: {jobs:?}");

        let post = routine_post(&mut conn, open.id, "https://marian.example").unwrap().unwrap();
        assert!(post.results);
        assert_eq!(post.reply_to.as_deref(), Some("555"));
        assert_eq!(post.message_id, None, "no reply yet, so one is sent");
        assert_eq!(post.title, "Poll results: Game night");
        assert!(post.body.starts_with("**Thursday, Oct 15** won with 2 votes."), "{}", post.body);
        assert!(post.buttons.is_empty());
        assert!(!post.body.contains("[Edit]"), "no edit link on the results: {}", post.body);
        assert_eq!(post.fields[0].value, "Nell\nJune");
        assert_eq!(post.fields[1].value, "June");

        remember_results_message(&mut conn, open.id, "777").unwrap();
        let post = routine_post(&mut conn, open.id, "https://marian.example").unwrap().unwrap();
        assert_eq!(post.message_id.as_deref(), Some("777"), "a second run edits the reply instead of posting again");

        // With a thread on the event, the results go in it rather than under the post.
        conn.batch_execute(&format!("UPDATE events SET thread_enabled = false WHERE id = {event_id}"))
            .unwrap();
        let post = routine_post(&mut conn, open.id, "https://marian.example").unwrap().unwrap();
        assert_eq!(post.thread, None, "no thread, so the results reply to the post");
        conn.batch_execute(&format!("UPDATE events SET thread_enabled = true WHERE id = {event_id}"))
            .unwrap();
        let post = routine_post(&mut conn, open.id, "https://marian.example").unwrap().unwrap();
        let thread = post.thread.expect("the event's thread");
        assert_eq!(thread.event_id, event_id);
        assert_eq!(thread.discord_id, None, "not open yet, so the results open it");
        assert!(thread.name.starts_with("Game night -- "), "{}", thread.name);
        store::set_thread_discord_id(&mut conn, event_id, &thread.name, "888").unwrap();
        let post = routine_post(&mut conn, open.id, "https://marian.example").unwrap().unwrap();
        assert_eq!(post.thread.unwrap().discord_id.as_deref(), Some("888"));
    }

    #[test]
    fn an_open_poll_lists_people_beside_each_day() {
        let mut conn = world();
        conn.batch_execute("UPDATE channels SET discord_id = '123' WHERE id = 1")
            .unwrap();
        let (id, public_id, _) = create_routine(
            &mut conn,
            1,
            1,
            1,
            "Game night",
            "",
            120,
            19 * 60,
            6,
            10 * 60,
            1,
            18 * 60,
            &[3, 4],
            None,
            at(2026, 10, 12, 12, 0),
        )
        .unwrap();
        let open = editable_run(&mut conn, id).unwrap().unwrap();
        cast_run(&mut conn, open.id, 1, 3).unwrap();
        cast_run(&mut conn, open.id, 2, 3).unwrap();
        let shown = routine_page_data(&mut conn, id).unwrap().unwrap();
        let thursday = shown.run.unwrap().days.into_iter().find(|day| day.n == 3).unwrap();
        assert_eq!(thursday.voters, ["Nell", "June"], "the page names who voted, as the post does");
        assert!(shown.usual.iter().all(|day| day.voters.is_empty()));
        let rows = list_routines(&mut conn, 1).unwrap();
        assert_eq!(rows[0].badge, "Voting");
        assert_eq!(rows[0].poll, "Poll locks Tue, Oct 13 · 6:00 PM");
        assert_eq!(rows[0].event, "");
        assert_eq!(rows[0].choices[0].label, "Thu, Oct 15");
        assert_eq!(rows[0].choices[0].votes, 2);
        assert_eq!(rows[0].choices[1].label, "Fri, Oct 16");
        assert_eq!(rows[0].choices[1].votes, 0);
        let post = routine_post(&mut conn, open.id, "https://marian.example")
            .unwrap()
            .unwrap();
        assert_eq!(post.title, "Next event: Game night");
        let locks = format!("Locks <t:{}:R>.", open.locks_at.timestamp());
        assert!(post.body.starts_with(&locks), "Discord counts down to the lock: {post:?}");
        let mut settings = store::settings(&mut conn, 1).unwrap();
        settings.localize_time = false;
        store::update_settings(&mut conn, &settings).unwrap();
        let post = routine_post(&mut conn, open.id, "https://marian.example")
            .unwrap()
            .unwrap();
        assert!(
            post.body.starts_with("Locks Tue, Oct 13 · 6:00 PM."),
            "without localized times the server's zone is spelled out: {post:?}"
        );
        assert!(
            post.body.contains(&format!(
                "[Edit](https://marian.example/servers/99/routines/{public_id})"
            )),
            "{post:?}"
        );
        assert!(!post.body.contains("Thursday"), "{post:?}");
        assert_eq!(post.fields.len(), 2);
        assert_eq!(post.fields[0].name, "**Thursday, Oct 15** (2)");
        assert_eq!(post.fields[0].value, "Nell\nJune");
        assert!(post.fields[0].inline);
        assert_eq!(post.fields[1].name, "**Friday, Oct 16** (0)");
        assert_eq!(post.fields[1].value, "-");
        assert!(post.fields[1].inline);
        assert_eq!(post.buttons[0].label, "Thursday, Oct 15");
        // The button names the poll by its public id, which leads back to the poll.
        let run = post.buttons[0].custom_id.split(':').nth(2).unwrap();
        let run = uuid::Uuid::parse_str(run).expect("a public id, not a row number");
        assert_eq!(run.get_version_num(), 7);
        assert_eq!(run_by_public(&mut conn, run).unwrap(), open_run_id(&mut conn, id).unwrap());
    }
}
