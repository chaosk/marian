//! When Marian does what. Each thing that happens at a set time is one row in
//! `tasks`: post an event, send a reminder, close signups, archive, move a
//! routine on, send a digest, fill series, sync someone's Google Calendar.
//!
//! Postgres triggers mark what changed in `task_marks` and notify
//! `marian_tasks`, whichever code made the change. The leader's worker turns
//! marks into task times (`drain`), runs what is due (`run_next`), and sleeps
//! until the next task or the next mark.
//!
//! A task runs in one transaction under its server's tick lock, the one the
//! old five-second tick took: the task row is claimed `FOR UPDATE SKIP
//! LOCKED` and deleted, the work is done, its Discord jobs are saved, and the
//! subject's next tasks are set, all or nothing. Two workers overlapping
//! while leadership changes hands take turns per server, and the second
//! finds the task gone or no longer due.

use chrono::{DateTime, Duration, NaiveDate, SubsecRound, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Timestamptz};
use diesel::upsert::excluded;
use diesel_async::RunQueryDsl;

use super::model::TickReport;
use super::tick;
use crate::clock;
use crate::error::OpError;
use crate::schema::{self, tasks};
use crate::store::{self, Connection};
use crate::values::{Digest, EventStatus, MarkKind, RunStatus, TaskKind};

/// How long an active routine goes unchecked at most, in minutes, in case
/// time alone moved it on.
const ROUTINE_CHECK: i64 = 60;
/// How often a server's series are filled while it has active ones, in
/// minutes, as their horizon moves on.
const FILL_EVERY: i64 = 60;
/// How long a Google Calendar goes without a full pass at most, in minutes.
const GOOGLE_CHECK: i64 = 6 * 60;
/// How long a Google sync has its task to itself, in minutes, before another
/// worker may take it, should this one have gone away.
const GOOGLE_LEASE: i64 = 10;
/// How long a post that failed waits before the next try, in seconds: the
/// old tick's interval.
const POST_RETRY: i64 = 5;
/// How long a task that failed waits before it's tried again, in minutes.
const FAILED_RETRY: i64 = 1;

/// What a worker pass did, and when it should look again.
pub struct Pass {
    pub report: TickReport,
    /// Tasks run, failed ones included.
    pub ran: usize,
    /// Marks turned into task times.
    pub marks: usize,
    /// When the earliest task not run yet is due, Google's aside.
    pub next: Option<DateTime<Utc>>,
}

/// Turns marks into task times, then runs up to `limit` due tasks, turning
/// the marks each leaves into task times too.
pub async fn run_tasks(
    conn: &mut Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<Pass, OpError> {
    let mut pass = Pass {
        report: tick::empty_report(),
        ran: 0,
        marks: drain(conn, now).await?,
        next: None,
    };
    while pass.ran < limit {
        let Some(report) = run_next(conn, now).await? else {
            break;
        };
        pass.ran += 1;
        merge(&mut pass.report, report);
        pass.marks += drain(conn, now).await?;
    }
    pass.next = next_due(conn).await?;
    Ok(pass)
}

/// Everything due by `now`, until nothing is left: what `tick` does.
pub async fn catch_up(conn: &mut Connection, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = tick::empty_report();
    loop {
        drain(conn, now).await?;
        match run_next(conn, now).await? {
            Some(piece) => merge(&mut report, piece),
            None => return Ok(report),
        }
    }
}

fn merge(into: &mut TickReport, piece: TickReport) {
    into.notes.extend(piece.notes);
    into.jobs.extend(piece.jobs);
    for guild in piece.guilds {
        if !into.guilds.contains(&guild) {
            into.guilds.push(guild);
        }
    }
}

/// Marks every server and Google Calendar, so the next drain sets all their
/// tasks from scratch. The worker does this as each leader's term starts.
pub async fn mark_everything(conn: &mut Connection) -> Result<(), OpError> {
    diesel::sql_query(
        "INSERT INTO task_marks (kind, subject_id) \
         SELECT 'guild', id FROM guilds \
         UNION ALL SELECT 'google', user_id FROM google_accounts \
         ON CONFLICT DO NOTHING",
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// When the earliest task the worker runs is due.
pub async fn next_due(conn: &mut Connection) -> Result<Option<DateTime<Utc>>, OpError> {
    Ok(tasks::table
        .filter(tasks::kind.ne(TaskKind::GoogleSync))
        .select(diesel::dsl::min(tasks::run_at))
        .first(conn)
        .await?)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Claimed {
    id: i64,
    pub(super) kind: TaskKind,
    pub(super) subject: i64,
    guild: Option<i64>,
}

/// Runs the task due soonest, if any, in a transaction of its own under its
/// server's tick lock. A task that fails is logged and tried again a minute
/// later; the error comes back only when that couldn't be recorded either.
pub async fn run_next(
    conn: &mut Connection,
    now: DateTime<Utc>,
) -> Result<Option<TickReport>, OpError> {
    // Whose task is due first, read without a lock: the server's lock comes
    // before the task's row, so two workers take turns rather than deadlock.
    let Some(guild) = tasks::table
        .filter(tasks::kind.ne(TaskKind::GoogleSync))
        .filter(tasks::run_at.le(now))
        .order((tasks::run_at.asc(), tasks::id.asc()))
        .select(tasks::guild_id)
        .first::<Option<i64>>(conn)
        .await
        .optional()?
    else {
        return Ok(None);
    };
    let mut claimed = None;
    let ran = store::with_tx(conn, async |tx| {
        if let Some(guild_id) = guild {
            store::lock_tick(tx, guild_id).await?;
        }
        let Some(task) = claim(tx, guild, now).await? else {
            return Ok(tick::empty_report());
        };
        claimed = Some(task);
        let report = perform(tx, task.kind, task.subject, now).await?;
        // A task that just ran isn't due again in the same instant.
        diesel::update(
            tasks::table
                .filter(tasks::kind.eq(task.kind))
                .filter(tasks::subject_id.eq(task.subject))
                .filter(tasks::run_at.le(now)),
        )
        .set(tasks::run_at.eq(now + Duration::minutes(FAILED_RETRY)))
        .execute(tx)
        .await?;
        // Saved with the task's changes: an event marked posted, or a
        // reminder marked sent, always has its Discord job waiting.
        crate::jobs::save(tx, &report.jobs).await?;
        Ok(report)
    });
    match ran.await {
        Ok(report) => Ok(Some(report)),
        Err(err) => {
            let Some(task) = claimed else {
                return Err(err);
            };
            tracing::warn!(
                guild_id = task.guild,
                "task {} for {} failed, trying again in a minute: {err}",
                task.kind,
                task.subject
            );
            diesel::update(tasks::table.find(task.id))
                .set(tasks::run_at.eq(now + Duration::minutes(FAILED_RETRY)))
                .execute(conn)
                .await?;
            Ok(Some(tick::empty_report()))
        }
    }
}

#[derive(QueryableByName)]
struct Taken {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Text)]
    kind: TaskKind,
    #[diesel(sql_type = BigInt)]
    subject_id: i64,
}

/// Takes `guild`'s task due soonest for this transaction: deleted, so it's
/// done once, and locked until then, so another worker skips it.
pub(super) async fn claim(
    conn: &mut Connection,
    guild: Option<i64>,
    now: DateTime<Utc>,
) -> Result<Option<Claimed>, OpError> {
    let taken = diesel::sql_query(
        "DELETE FROM tasks WHERE id = \
         (SELECT id FROM tasks WHERE kind <> 'google_sync' AND run_at <= $1 \
          AND guild_id IS NOT DISTINCT FROM $2 \
          ORDER BY run_at, id LIMIT 1 FOR UPDATE SKIP LOCKED) \
         RETURNING id, kind, subject_id",
    )
    .bind::<Timestamptz, _>(now)
    .bind::<diesel::sql_types::Nullable<BigInt>, _>(guild)
    .get_result::<Taken>(conn)
    .await
    .optional()?;
    Ok(taken.map(|task| Claimed {
        id: task.id,
        kind: task.kind,
        subject: task.subject_id,
        guild,
    }))
}

/// Does one task, then sets when its subject is next due, whether or not
/// the task changed anything a trigger would see.
pub(super) async fn perform(
    conn: &mut Connection,
    kind: TaskKind,
    subject: i64,
    now: DateTime<Utc>,
) -> Result<TickReport, OpError> {
    match kind {
        TaskKind::Post | TaskKind::CloseSignups | TaskKind::Archive => {
            let report = match kind {
                TaskKind::Post => tick::post_one(conn, subject, now).await?,
                TaskKind::CloseSignups => tick::close_one(conn, subject, now).await?,
                _ => tick::archive_one(conn, subject, now).await?,
            };
            sync_event(conn, subject, now).await?;
            Ok(report)
        }
        TaskKind::Remind => {
            let report = tick::remind_one(conn, subject, now).await?;
            let event: Option<i64> = schema::reminders::table
                .find(subject)
                .select(schema::reminders::event_id)
                .first(conn)
                .await
                .optional()?;
            if let Some(event_id) = event {
                sync_event(conn, event_id, now).await?;
            }
            Ok(report)
        }
        TaskKind::Routine => {
            let mut report = tick::empty_report();
            if let Some((_, guild_id)) = routine_state(conn, subject).await? {
                report.jobs = super::routine::kick(conn, subject, now).await?;
                if !report.jobs.is_empty() {
                    report.guilds.push(guild_id);
                }
            }
            sync_routine(conn, subject, now, true).await?;
            Ok(report)
        }
        TaskKind::Digest => {
            let report = tick::digest_one(conn, subject, now).await?;
            sync_clock(conn, subject, now).await?;
            Ok(report)
        }
        TaskKind::FillSeries => {
            tick::fill_guild(conn, subject, now).await?;
            if active_series(conn, subject).await? {
                put(
                    conn,
                    TaskKind::FillSeries,
                    subject,
                    Some(subject),
                    now + Duration::minutes(FILL_EVERY),
                )
                .await?;
            }
            Ok(tick::empty_report())
        }
        // The Google loop runs these, apart from the rest.
        TaskKind::GoogleSync => Ok(tick::empty_report()),
    }
}

#[derive(QueryableByName)]
struct Mark {
    #[diesel(sql_type = Text)]
    kind: MarkKind,
    #[diesel(sql_type = BigInt)]
    subject_id: i64,
}

/// Turns what changed into task times, one mark per transaction, and says
/// how many there were. A mark that can't be applied is logged and dropped;
/// the full pass at the next leader's start sets its tasks again.
pub async fn drain(conn: &mut Connection, now: DateTime<Utc>) -> Result<usize, OpError> {
    let mut count = 0;
    loop {
        let took = store::with_tx(conn, async |tx| {
            let Some(mark) = diesel::sql_query(
                "DELETE FROM task_marks WHERE (kind, subject_id) = \
                 (SELECT kind, subject_id FROM task_marks LIMIT 1 FOR UPDATE SKIP LOCKED) \
                 RETURNING kind, subject_id",
            )
            .get_result::<Mark>(tx)
            .await
            .optional()?
            else {
                return Ok(false);
            };
            // A savepoint: a mark that can't be applied leaves the rest be.
            let applied = store::with_tx(tx, async |sp| {
                apply(sp, mark.kind, mark.subject_id, now).await
            })
            .await;
            if let Err(err) = applied {
                tracing::warn!(
                    "couldn't set the tasks for {} {}: {err}",
                    mark.kind,
                    mark.subject_id
                );
            }
            Ok(true)
        })
        .await?;
        if !took {
            break;
        }
        count += 1;
    }
    if count > 0 {
        // A reminder that was deleted or sent has nothing left to do.
        diesel::sql_query(
            "DELETE FROM tasks WHERE kind = 'remind' AND NOT EXISTS \
             (SELECT 1 FROM reminders WHERE reminders.id = tasks.subject_id \
              AND reminders.sent_at IS NULL)",
        )
        .execute(conn)
        .await?;
    }
    Ok(count)
}

async fn apply(
    conn: &mut Connection,
    kind: MarkKind,
    subject: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    match kind {
        MarkKind::Event => sync_event(conn, subject, now).await,
        MarkKind::Routine => sync_routine(conn, subject, now, false).await,
        MarkKind::Guild => sync_guild(conn, subject, now).await,
        MarkKind::Fill => sync_fill(conn, subject, now).await,
        MarkKind::Google => note_google(conn, subject, now).await,
        MarkKind::Member => {
            let user: Option<i64> = schema::memberships::table
                .find(subject)
                .select(schema::memberships::user_id)
                .first(conn)
                .await
                .optional()?;
            match user {
                Some(user_id) => note_google(conn, user_id, now).await,
                None => Ok(()),
            }
        }
        MarkKind::Channel => {
            let guild: Option<i64> = schema::channels::table
                .find(subject)
                .select(schema::channels::guild_id)
                .first(conn)
                .await
                .optional()?;
            match guild {
                Some(guild_id) => note_people(conn, guild_id, now).await,
                None => Ok(()),
            }
        }
        MarkKind::People => note_people(conn, subject, now).await,
    }
}

/// An event's post, reminders, signup cutoff and archive, from the event as
/// it is, and a sync for everyone whose calendar it is or was on.
pub async fn sync_event(
    conn: &mut Connection,
    event_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let Some(event) = store::event(conn, event_id).await? else {
        for kind in [TaskKind::Post, TaskKind::CloseSignups, TaskKind::Archive] {
            drop_task(conn, kind, event_id).await?;
        }
        return note_linked_google(conn, event_id, now).await;
    };
    let guild = Some(event.guild_id);
    let post = event
        .post_at
        .filter(|_| event.status == EventStatus::Scheduled && event.post_attempts < 3)
        // A failed try waits a moment before the next, as the tick did.
        .map(|at| match event.post_attempts {
            0 => at,
            _ => at.max(event.updated_at + Duration::seconds(POST_RETRY)),
        });
    match post {
        Some(at) => put(conn, TaskKind::Post, event.id, guild, at).await?,
        None => drop_task(conn, TaskKind::Post, event.id).await?,
    }
    let posted = event.status == EventStatus::Posted;
    for rem in store::reminders_for(conn, &event).await? {
        if posted && rem.sent_at.is_none() {
            let at = event.starts_at - Duration::minutes(rem.offset_minutes);
            put(conn, TaskKind::Remind, rem.id, guild, at).await?;
        } else {
            drop_task(conn, TaskKind::Remind, rem.id).await?;
        }
    }
    match event.cutoff_at.filter(|_| posted && !event.signups_closed) {
        Some(cutoff) => put(conn, TaskKind::CloseSignups, event.id, guild, cutoff).await?,
        None => drop_task(conn, TaskKind::CloseSignups, event.id).await?,
    }
    let settings = store::settings(conn, event.guild_id).await?;
    match tick::archive_at(&event, &settings).filter(|_| posted) {
        Some(at) => put(conn, TaskKind::Archive, event.id, guild, at).await?,
        None => drop_task(conn, TaskKind::Archive, event.id).await?,
    }
    // Whoever has the event in their calendar, or might now.
    if let Some(created_by) = event.created_by
        && let Some(member) = store::membership(conn, created_by).await?
    {
        note_google(conn, member.user_id, now).await?;
    }
    let signed: Vec<i64> = schema::signups::table
        .inner_join(schema::memberships::table)
        .filter(schema::signups::event_id.eq(event.id))
        .select(schema::memberships::user_id)
        .distinct()
        .load(conn)
        .await?;
    for user_id in signed {
        note_google(conn, user_id, now).await?;
    }
    note_linked_google(conn, event.id, now).await
}

/// A routine run's status, when it opens and locks, and when its approval lapses.
type RunTimes = (
    RunStatus,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

/// When a routine next needs a look: a poll to open or lock, an approval to
/// lapse, or at most an hour while it's active. `kicked`: it was just moved
/// on, so with no poll on the way this week's is settled and the next one
/// starts as this window closes; otherwise it's looked at now.
async fn sync_routine(
    conn: &mut Connection,
    routine_id: i64,
    now: DateTime<Utc>,
    kicked: bool,
) -> Result<(), OpError> {
    let Some((active, guild_id)) = routine_state(conn, routine_id).await? else {
        return drop_task(conn, TaskKind::Routine, routine_id).await;
    };
    let runs: Vec<RunTimes> = schema::routine_runs::table
        .filter(schema::routine_runs::routine_id.eq(routine_id))
        .select((
            schema::routine_runs::status,
            schema::routine_runs::opens_at,
            schema::routine_runs::locks_at,
            schema::routine_runs::decide_by,
        ))
        .load(conn)
        .await?;
    let mut next: Option<DateTime<Utc>> = None;
    let mut soonest = |at: DateTime<Utc>| next = Some(next.map_or(at, |next| next.min(at)));
    let mut current = false;
    for (status, opens, locks, decide_by) in &runs {
        match status {
            RunStatus::Scheduled => {
                current = true;
                soonest(*opens);
            }
            RunStatus::Open => {
                current = true;
                soonest(*locks);
            }
            RunStatus::Approval => {
                current = true;
                if let Some(at) = decide_by {
                    soonest(*at);
                }
            }
            RunStatus::Locked | RunStatus::Skipped | RunStatus::Expired => {}
        }
    }
    if active {
        let check = now + Duration::minutes(ROUTINE_CHECK);
        if current {
            soonest(check);
        } else if kicked {
            let end = super::routine::window_end(conn, routine_id, now).await?;
            soonest(end.map_or(check, |end| end.min(check)));
        } else {
            soonest(now);
        }
    }
    match next {
        Some(at) => put(conn, TaskKind::Routine, routine_id, Some(guild_id), at).await,
        None => drop_task(conn, TaskKind::Routine, routine_id).await,
    }
}

/// Whether a routine is active, and its server; `None` once it's gone.
async fn routine_state(
    conn: &mut Connection,
    routine_id: i64,
) -> Result<Option<(bool, i64)>, OpError> {
    Ok(schema::routines::table
        .find(routine_id)
        .select((schema::routines::active, schema::routines::guild_id))
        .first(conn)
        .await
        .optional()?)
}

/// A server's digest and series fill, and every event and routine in it.
async fn sync_guild(
    conn: &mut Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    if store::guild(conn, guild_id).await?.is_none() {
        drop_task(conn, TaskKind::Digest, guild_id).await?;
        return drop_task(conn, TaskKind::FillSeries, guild_id).await;
    }
    sync_clock(conn, guild_id, now).await?;
    let events: Vec<i64> = schema::events::table
        .filter(schema::events::guild_id.eq(guild_id))
        .filter(schema::events::status.eq_any([EventStatus::Posted, EventStatus::Scheduled]))
        .select(schema::events::id)
        .load(conn)
        .await?;
    for event_id in events {
        sync_event(conn, event_id, now).await?;
    }
    let routines: Vec<i64> = schema::routines::table
        .filter(schema::routines::guild_id.eq(guild_id))
        .select(schema::routines::id)
        .load(conn)
        .await?;
    for routine_id in routines {
        sync_routine(conn, routine_id, now, false).await?;
    }
    note_people(conn, guild_id, now).await
}

/// A server's digest, and its series fill while it has active series.
async fn sync_clock(
    conn: &mut Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let settings = store::settings(conn, guild_id).await?;
    match next_digest(conn, guild_id, settings.digest, now).await? {
        Some(at) => put(conn, TaskKind::Digest, guild_id, Some(guild_id), at).await?,
        None => drop_task(conn, TaskKind::Digest, guild_id).await?,
    }
    if active_series(conn, guild_id).await? {
        // The hourly fill stays where it is; a missing one fills now.
        put_if_missing(conn, TaskKind::FillSeries, guild_id, Some(guild_id), now).await
    } else {
        drop_task(conn, TaskKind::FillSeries, guild_id).await
    }
}

/// A server's series changed, or one of their dates did: fill them now.
async fn sync_fill(
    conn: &mut Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    if active_series(conn, guild_id).await? {
        put_soonest(conn, TaskKind::FillSeries, guild_id, Some(guild_id), now).await
    } else {
        drop_task(conn, TaskKind::FillSeries, guild_id).await
    }
}

/// When a server's next digest goes out: 9:00 in its zone on a digest day,
/// or now when today's is late and not yet sent.
async fn next_digest(
    conn: &mut Connection,
    guild_id: i64,
    digest: Digest,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, OpError> {
    if digest == Digest::Off {
        return Ok(None);
    }
    let guild = store::guild(conn, guild_id)
        .await?
        .ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let today = now.with_timezone(&tz).date_naive();
    for offset in 0..8 {
        let day = today + Duration::days(offset);
        if !tick::digest_day(digest, day) {
            continue;
        }
        let at = nine_on(day, tz);
        if offset == 0 {
            if store::meta(conn, &tick::digest_key(guild_id, day))
                .await?
                .is_some()
            {
                continue;
            }
            return Ok(Some(at.max(now)));
        }
        return Ok(Some(at));
    }
    Ok(None)
}

fn nine_on(day: NaiveDate, tz: Tz) -> DateTime<Utc> {
    marian_types::time::local_to_utc(day.and_hms_opt(9, 0, 0).expect("9:00 exists"), tz)
}

async fn active_series(conn: &mut Connection, guild_id: i64) -> Result<bool, OpError> {
    Ok(diesel::select(diesel::dsl::exists(
        schema::series::table
            .filter(schema::series::guild_id.eq(guild_id))
            .filter(schema::series::active.eq(true)),
    ))
    .get_result(conn)
    .await?)
}

async fn google_connected(conn: &mut Connection, user_id: i64) -> Result<bool, OpError> {
    Ok(diesel::select(diesel::dsl::exists(
        schema::google_accounts::table.filter(schema::google_accounts::user_id.eq(user_id)),
    ))
    .get_result(conn)
    .await?)
}

/// A Google Calendar sync for `user_id` now, when they have one.
async fn note_google(
    conn: &mut Connection,
    user_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    if google_connected(conn, user_id).await? {
        put_soonest(conn, TaskKind::GoogleSync, user_id, None, now).await?;
    }
    Ok(())
}

/// Everyone with an event in their calendar, so a removed event leaves it.
async fn note_linked_google(
    conn: &mut Connection,
    event_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let users: Vec<i64> = schema::google_events::table
        .filter(schema::google_events::event_id.eq(event_id))
        .select(schema::google_events::user_id)
        .load(conn)
        .await?;
    for user_id in users {
        note_google(conn, user_id, now).await?;
    }
    Ok(())
}

/// Everyone in a server with a calendar: who sees what there changed.
async fn note_people(
    conn: &mut Connection,
    guild_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let users: Vec<i64> = schema::memberships::table
        .inner_join(
            schema::google_accounts::table
                .on(schema::google_accounts::user_id.eq(schema::memberships::user_id)),
        )
        .filter(schema::memberships::guild_id.eq(guild_id))
        .select(schema::memberships::user_id)
        .distinct()
        .load(conn)
        .await?;
    for user_id in users {
        put_soonest(conn, TaskKind::GoogleSync, user_id, None, now).await?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct GoogleClaim {
    #[diesel(sql_type = BigInt)]
    subject_id: i64,
}

/// Takes the Google sync due soonest at `now`, if any: the user, and the
/// lease it holds until `finish_google`. No other worker takes it before the
/// lease is up.
pub async fn claim_google(
    conn: &mut Connection,
    now: DateTime<Utc>,
) -> Result<Option<(i64, DateTime<Utc>)>, OpError> {
    let lease = (now + Duration::minutes(GOOGLE_LEASE)).trunc_subsecs(0);
    let claimed = diesel::sql_query(
        "UPDATE tasks SET run_at = $1 WHERE id = \
         (SELECT id FROM tasks WHERE kind = 'google_sync' AND run_at <= $2 \
          ORDER BY run_at, id LIMIT 1 FOR UPDATE SKIP LOCKED) \
         RETURNING subject_id",
    )
    .bind::<Timestamptz, _>(lease)
    .bind::<Timestamptz, _>(now)
    .get_result::<GoogleClaim>(conn)
    .await
    .optional()?;
    Ok(claimed.map(|claim| (claim.subject_id, lease)))
}

/// After a Google sync: again at `again`, or the full pass later. A change
/// marked while it ran keeps its sooner time.
pub async fn finish_google(
    conn: &mut Connection,
    user_id: i64,
    lease: DateTime<Utc>,
    again: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    if !google_connected(conn, user_id).await? {
        return drop_task(conn, TaskKind::GoogleSync, user_id).await;
    }
    let check = now + Duration::minutes(GOOGLE_CHECK);
    let next = again.map_or(check, |at| at.min(check));
    let updated = diesel::sql_query(
        "UPDATE tasks SET run_at = CASE WHEN run_at = $2 THEN $3 ELSE LEAST(run_at, $3) END \
         WHERE kind = 'google_sync' AND subject_id = $1",
    )
    .bind::<BigInt, _>(user_id)
    .bind::<Timestamptz, _>(lease)
    .bind::<Timestamptz, _>(next)
    .execute(conn)
    .await?;
    if updated == 0 {
        put_soonest(conn, TaskKind::GoogleSync, user_id, None, next).await?;
    }
    Ok(())
}

/// When the earliest Google sync is due.
pub async fn next_google(conn: &mut Connection) -> Result<Option<DateTime<Utc>>, OpError> {
    Ok(tasks::table
        .filter(tasks::kind.eq(TaskKind::GoogleSync))
        .select(diesel::dsl::min(tasks::run_at))
        .first(conn)
        .await?)
}

type NewTask = (
    diesel::dsl::Eq<tasks::kind, TaskKind>,
    diesel::dsl::Eq<tasks::subject_id, i64>,
    diesel::dsl::Eq<tasks::guild_id, Option<i64>>,
    diesel::dsl::Eq<tasks::run_at, DateTime<Utc>>,
);

fn new_task(kind: TaskKind, subject: i64, guild: Option<i64>, at: DateTime<Utc>) -> NewTask {
    (
        tasks::kind.eq(kind),
        tasks::subject_id.eq(subject),
        tasks::guild_id.eq(guild),
        tasks::run_at.eq(at),
    )
}

async fn put(
    conn: &mut Connection,
    kind: TaskKind,
    subject: i64,
    guild: Option<i64>,
    at: DateTime<Utc>,
) -> Result<(), OpError> {
    diesel::insert_into(tasks::table)
        .values(new_task(kind, subject, guild, at))
        .on_conflict((tasks::kind, tasks::subject_id))
        .do_update()
        .set((
            tasks::run_at.eq(excluded(tasks::run_at)),
            tasks::guild_id.eq(excluded(tasks::guild_id)),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

/// Like `put`, but a task already due sooner stays as it is.
async fn put_soonest(
    conn: &mut Connection,
    kind: TaskKind,
    subject: i64,
    guild: Option<i64>,
    at: DateTime<Utc>,
) -> Result<(), OpError> {
    diesel::insert_into(tasks::table)
        .values(new_task(kind, subject, guild, at))
        .on_conflict((tasks::kind, tasks::subject_id))
        .do_update()
        .set(tasks::run_at.eq(schema::least(tasks::run_at, excluded(tasks::run_at))))
        .execute(conn)
        .await?;
    Ok(())
}

async fn put_if_missing(
    conn: &mut Connection,
    kind: TaskKind,
    subject: i64,
    guild: Option<i64>,
    at: DateTime<Utc>,
) -> Result<(), OpError> {
    diesel::insert_into(tasks::table)
        .values(new_task(kind, subject, guild, at))
        .on_conflict((tasks::kind, tasks::subject_id))
        .do_nothing()
        .execute(conn)
        .await?;
    Ok(())
}

async fn drop_task(conn: &mut Connection, kind: TaskKind, subject: i64) -> Result<(), OpError> {
    diesel::delete(
        tasks::table
            .filter(tasks::kind.eq(kind))
            .filter(tasks::subject_id.eq(subject)),
    )
    .execute(conn)
    .await?;
    Ok(())
}
