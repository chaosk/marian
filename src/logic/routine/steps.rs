//! A routine's steps: a date poll first, the event's post last, and an
//! approval in between when someone has to confirm the day before it posts.

use chrono::TimeZone;

use super::*;
use crate::logic::access::holds_any;
use crate::logic::model::ButtonTone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// The weekly poll for the day.
    Poll,
    /// Asks some roles in a channel to confirm the day, pick another, or skip.
    Approval,
    /// Posts the event, taking over the poll's message.
    Post,
}

impl StepKind {
    pub fn code(self) -> &'static str {
        match self {
            StepKind::Poll => "poll",
            StepKind::Approval => "approval",
            StepKind::Post => "post",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "poll" => Some(StepKind::Poll),
            "approval" => Some(StepKind::Approval),
            "post" => Some(StepKind::Post),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    /// Where an approval asks.
    pub channel_id: Option<i64>,
    /// Who an approval mentions and lets answer, besides server managers and
    /// the routine's creator.
    pub role_ids: Vec<i64>,
}

impl Step {
    pub fn of(kind: StepKind) -> Self {
        Step {
            kind,
            channel_id: None,
            role_ids: Vec::new(),
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::routine_steps)]
struct NewStep<'a> {
    routine_id: i64,
    position: i64,
    kind: &'a str,
    channel_id: Option<i64>,
    role_ids: &'a [i64],
}

/// What every routine did before it had steps: poll, then post.
pub fn default_steps() -> Vec<Step> {
    vec![Step::of(StepKind::Poll), Step::of(StepKind::Post)]
}

pub fn load_steps(conn: &mut Connection, routine_id: i64) -> Result<Vec<Step>, OpError> {
    let rows: Vec<(String, Option<i64>, Vec<i64>)> = schema::routine_steps::table
        .filter(schema::routine_steps::routine_id.eq(routine_id))
        .order(schema::routine_steps::position.asc())
        .select((
            schema::routine_steps::kind,
            schema::routine_steps::channel_id,
            schema::routine_steps::role_ids,
        ))
        .load(conn)?;
    let steps: Vec<Step> = rows
        .into_iter()
        .filter_map(|(kind, channel_id, role_ids)| {
            Some(Step {
                kind: StepKind::from_code(&kind)?,
                channel_id,
                role_ids,
            })
        })
        .collect();
    Ok(if steps.is_empty() { default_steps() } else { steps })
}

/// Checks a routine's steps: the poll first, the post last, and at most one
/// approval between them, asking in a channel of this server.
pub(super) fn clean_steps(conn: &mut Connection, guild_id: i64, steps: &[Step]) -> Result<Vec<Step>, OpError> {
    let shape_ok = steps.len() >= 2
        && steps.first().map(|step| step.kind) == Some(StepKind::Poll)
        && steps.last().map(|step| step.kind) == Some(StepKind::Post)
        && steps[1..steps.len() - 1].iter().all(|step| step.kind == StepKind::Approval);
    if !shape_ok {
        return Err(OpError::message("A routine polls first and posts the event last, with approvals in between."));
    }
    if steps.len() > 3 {
        return Err(OpError::message("A routine can have one approval step for now."));
    }
    let known: Vec<i64> = store::list_roles(conn, guild_id)?.into_iter().map(|role| role.id).collect();
    let mut clean = Vec::new();
    for step in steps {
        if step.kind != StepKind::Approval {
            clean.push(Step::of(step.kind));
            continue;
        }
        let channel_id = step
            .channel_id
            .ok_or_else(|| OpError::message("Pick the channel the approval asks in."))?;
        let channel = store::channel(conn, channel_id)?.ok_or(OpError::NotFound)?;
        if channel.guild_id != guild_id {
            return Err(OpError::NotFound);
        }
        let mut role_ids: Vec<i64> = step.role_ids.iter().copied().filter(|id| known.contains(id)).collect();
        role_ids.dedup();
        clean.push(Step {
            kind: StepKind::Approval,
            channel_id: Some(channel_id),
            role_ids,
        });
    }
    Ok(clean)
}

pub(super) fn store_steps(conn: &mut Connection, routine_id: i64, steps: &[Step]) -> Result<(), OpError> {
    diesel::delete(schema::routine_steps::table.filter(schema::routine_steps::routine_id.eq(routine_id)))
        .execute(conn)?;
    for (position, step) in steps.iter().enumerate() {
        diesel::insert_into(schema::routine_steps::table)
            .values(&NewStep {
                routine_id,
                position: position as i64,
                kind: step.kind.code(),
                channel_id: step.channel_id,
                role_ids: &step.role_ids,
            })
            .execute(conn)?;
    }
    Ok(())
}

/// Moves a run on from step `after` with `day` picked: to the next approval,
/// or to posting the event.
pub(super) fn proceed(
    conn: &mut Connection,
    routine: &Routine,
    run: &Run,
    day: u8,
    after: usize,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordJob>, OpError> {
    let steps = load_steps(conn, routine.id)?;
    let next = steps
        .iter()
        .enumerate()
        .skip(after + 1)
        .find(|(_, step)| step.kind != StepKind::Poll);
    match next {
        Some((position, step)) if step.kind == StepKind::Approval => ask_approval(conn, routine, run, day, position),
        _ => post_run(conn, routine, run, day, now),
    }
}

fn ask_approval(
    conn: &mut Connection,
    routine: &Routine,
    run: &Run,
    day: u8,
    position: usize,
) -> Result<Vec<DiscordJob>, OpError> {
    let tz = clock::zone(&must_guild(conn, routine.guild_id)?.timezone)?;
    let lapses = decide_by(run.locks_at, day, routine.start_minutes, tz);
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
        .set((
            schema::routine_runs::status.eq("approval"),
            schema::routine_runs::step.eq(position as i64),
            schema::routine_runs::winner_weekday.eq(i64::from(day)),
            schema::routine_runs::decide_by.eq(lapses),
        ))
        .execute(conn)?;
    log(
        conn,
        routine.guild_id,
        None,
        &format!(
            "The {} poll picked {}. Waiting for approval.",
            routine.title,
            clock::weekday_name(day)
        ),
    )?;
    Ok(vec![DiscordJob::routine(run.id)])
}

/// When an unanswered approval lapses: as the winning day begins, or at the
/// event's start when the poll closes on that day.
pub(super) fn decide_by(locks_at: DateTime<Utc>, day: u8, start_minutes: u16, tz: Tz) -> DateTime<Utc> {
    let start = event_start(locks_at, day, start_minutes, tz);
    let date = start.with_timezone(&tz).date_naive();
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .and_then(|local| tz.from_local_datetime(&local).earliest())
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or(start);
    if midnight > locks_at {
        midnight
    } else {
        start
    }
}

/// Lapses an approval nobody answered in time.
pub(super) fn expire(conn: &mut Connection, routine: &Routine, run: &Run) -> Result<Vec<DiscordJob>, OpError> {
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
        .set(schema::routine_runs::status.eq("expired"))
        .execute(conn)?;
    log(
        conn,
        routine.guild_id,
        None,
        &format!("No one approved {} in time, so there's no event this week.", routine.title),
    )?;
    Ok(vec![DiscordJob::routine(run.id)])
}

/// Whether someone may answer a routine's approval: server managers, the
/// routine's creator, and holders of the roles the step names.
fn may_decide(conn: &mut Connection, routine: &Routine, roles: &[i64], user: &store::Membership) -> Result<bool, OpError> {
    if user.server_manager || routine.created_by == Some(user.id) {
        return Ok(true);
    }
    Ok(holds_any(roles, &store::membership_role_ids(conn, user)?))
}

/// The approval step a run is waiting on, or answered.
fn approval_step(conn: &mut Connection, routine_id: i64, run: &Run) -> Result<Option<Step>, OpError> {
    Ok(load_steps(conn, routine_id)?
        .into_iter()
        .nth(run.step as usize)
        .filter(|step| step.kind == StepKind::Approval))
}

/// Whether `membership_id` may answer the approval `routine_id` waits on.
pub fn can_decide(conn: &mut Connection, routine_id: i64, membership_id: i64) -> Result<bool, OpError> {
    let routine = must_routine(conn, routine_id)?;
    let Some(run) = waiting_run(conn, routine_id)? else {
        return Ok(false);
    };
    let roles = approval_step(conn, routine_id, &run)?.map(|step| step.role_ids).unwrap_or_default();
    let user = must_membership(conn, membership_id)?;
    may_decide(conn, &routine, &roles, &user)
}

/// The run waiting for its approval, if there is one.
pub fn waiting_run_id(conn: &mut Connection, routine_id: i64) -> Result<Option<i64>, OpError> {
    Ok(waiting_run(conn, routine_id)?.map(|run| run.id))
}

pub(super) fn waiting_run(conn: &mut Connection, routine_id: i64) -> Result<Option<Run>, OpError> {
    Ok(runs_for(conn, routine_id)?.into_iter().find(|run| run.status == "approval"))
}

/// Answers an approval: post the event on `day`, or skip the week when there
/// is no day.
pub fn decide_run(
    conn: &mut Connection,
    run_id: i64,
    membership_id: i64,
    day: Option<u8>,
    now: DateTime<Utc>,
) -> Result<RoutineEffect, OpError> {
    store::with_tx(conn, |tx| {
        let run = must_run(tx, run_id)?;
        if run.status != "approval" {
            return Err(OpError::from(crate::msg!("err-approval-closed")));
        }
        let routine = must_routine(tx, run.routine_id)?;
        let user = must_membership(tx, membership_id)?;
        if user.guild_id != routine.guild_id {
            return Err(OpError::from(crate::msg!("err-person-elsewhere")));
        }
        let roles = approval_step(tx, routine.id, &run)?.map(|step| step.role_ids).unwrap_or_default();
        if !may_decide(tx, &routine, &roles, &user)? {
            return Err(OpError::from(crate::msg!("err-approval-not-yours")));
        }
        diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
            .set(schema::routine_runs::decided_by.eq(user.id))
            .execute(tx)?;
        let jobs = match day {
            None => {
                diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run.id)))
                    .set(schema::routine_runs::status.eq("skipped"))
                    .execute(tx)?;
                log(tx, routine.guild_id, None, &format!("{} skipped {} this week.", user.name, routine.title))?;
                vec![DiscordJob::routine(run.id)]
            }
            Some(day) => {
                if !run.days.contains(&day) {
                    return Err(OpError::from(crate::msg!("err-poll-day-missing")));
                }
                let tz = clock::zone(&must_guild(tx, routine.guild_id)?.timezone)?;
                if event_start(run.locks_at, day, routine.start_minutes, tz) <= now {
                    return Err(OpError::from(crate::msg!("err-approval-day-passed")));
                }
                log(
                    tx,
                    routine.guild_id,
                    None,
                    &format!("{} confirmed {} on {}.", user.name, routine.title, clock::weekday_name(day)),
                )?;
                proceed(tx, &routine, &run, day, run.step as usize, now)?
            }
        };
        Ok(RoutineEffect { jobs })
    })
}

#[derive(Debug, Clone)]
pub struct ApprovalPost {
    pub channel_discord_id: String,
    pub message_id: Option<String>,
    pub title: String,
    pub body: String,
    pub fields: Vec<DiscordField>,
    pub buttons: Vec<PollButton>,
    /// The roles the request pings while it waits.
    pub mention_roles: Vec<String>,
}

/// The request an approval step posts in its channel, and what it says once
/// answered. None when the run has no approval, or its channel isn't in Discord.
pub fn approval_post(
    conn: &mut Connection,
    run_id: i64,
    public_url: &str,
    now: DateTime<Utc>,
) -> Result<Option<ApprovalPost>, OpError> {
    let run = must_run(conn, run_id)?;
    if !matches!(run.status.as_str(), "approval" | "locked" | "skipped" | "expired") {
        return Ok(None);
    }
    let routine = must_routine(conn, run.routine_id)?;
    let Some(step) = approval_step(conn, routine.id, &run)? else {
        return Ok(None);
    };
    // A run that never asked has nothing to show.
    if run.status != "approval" && run.approval_message_id.is_none() {
        return Ok(None);
    }
    let Some(channel) = step.channel_id.map(|id| store::channel(conn, id)).transpose()?.flatten() else {
        return Ok(None);
    };
    let Some(discord_id) = channel.discord_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let guild = must_guild(conn, routine.guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    let settings = store::settings(conn, routine.guild_id)?;
    let lang = crate::i18n::Lang::from_code(&settings.language).unwrap_or_default();
    let counts = vote_counts(conn, run.id)?;
    let winner = run.winner_weekday.unwrap_or(run.days[0]);
    let label = |day: u8| {
        let when = event_start(run.locks_at, day, routine.start_minutes, tz);
        poll_day_label(lang, when.with_timezone(&tz).date_naive())
    };
    let decider = match run.decided_by {
        Some(id) => store::membership(conn, id)?.map(|person| person.name).unwrap_or_default(),
        None => String::new(),
    };
    let waiting = run.status == "approval";
    let body = match run.status.as_str() {
        "approval" => {
            let lapses = run.decide_by.unwrap_or(run.locks_at);
            let when = if settings.localize_time {
                format!("<t:{}:R>", lapses.timestamp())
            } else {
                crate::i18n::format_when(lang, lapses, tz, settings.hour_24)
            };
            crate::tr!(
                lang,
                "approval-ask",
                day = label(winner),
                count = counts.get(&winner).copied().unwrap_or(0),
                when = when,
            )
        }
        "locked" => crate::tr!(lang, "approval-posted", name = decider.as_str(), day = label(winner)),
        "skipped" => crate::tr!(lang, "approval-skipped", name = decider.as_str()),
        _ => crate::tr!(lang, "approval-expired"),
    };
    let body = if waiting {
        with_edit_link(body, public_url, guild.discord_id.as_deref(), routine.public_id, lang)
    } else {
        body
    };
    // Every day on the poll with its votes; days already begun can't be picked.
    let mut days: Vec<(DateTime<Utc>, u8)> = run
        .days
        .iter()
        .map(|day| (event_start(run.locks_at, *day, routine.start_minutes, tz), *day))
        .collect();
    days.sort();
    let fields = days
        .iter()
        .map(|(_, day)| DiscordField {
            name: label(*day),
            value: counts.get(day).copied().unwrap_or(0).to_string(),
            inline: true,
        })
        .collect();
    let buttons = if waiting {
        let mut buttons: Vec<PollButton> = days
            .iter()
            .filter(|(start, _)| *start > now)
            .map(|(_, day)| PollButton {
                custom_id: format!("marian:a:{}:{day}", run.public_id),
                label: format!("{} · {}", label(*day), counts.get(day).copied().unwrap_or(0)),
                tone: if *day == winner { ButtonTone::Primary } else { ButtonTone::Secondary },
            })
            .collect();
        buttons.push(PollButton {
            custom_id: format!("marian:a:{}:skip", run.public_id),
            label: crate::tr!(lang, "approval-skip"),
            tone: ButtonTone::Danger,
        });
        buttons
    } else {
        Vec::new()
    };
    let mention_roles = if waiting {
        mention_role_ids(conn, &step.role_ids)?
    } else {
        Vec::new()
    };
    Ok(Some(ApprovalPost {
        channel_discord_id: discord_id,
        message_id: run.approval_message_id,
        title: crate::tr!(lang, "approval-title", title = &routine.title),
        body,
        fields,
        buttons,
        mention_roles,
    }))
}

pub fn remember_approval_message(conn: &mut Connection, run_id: i64, message_id: &str) -> Result<(), OpError> {
    diesel::update(schema::routine_runs::table.filter(schema::routine_runs::id.eq(run_id)))
        .set(schema::routine_runs::approval_message_id.eq(message_id))
        .execute(conn)?;
    Ok(())
}
