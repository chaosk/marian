//! When Marian does what. Each thing that happens at a set time is one row in
//! `tasks`: post an event, send a reminder, close signups, archive, move a
//! routine on, send a digest, fill series, sync someone's Google Calendar.
//!
//! Postgres triggers mark what changed in `task_marks`, whichever code made
//! the change. The worker turns marks into task times, runs what is due, and
//! sleeps until the next task or the next mark.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use diesel::dsl::{exists, not};
use diesel::prelude::*;
use diesel::sql_types::Text;
use diesel::upsert::excluded;
use diesel::IntoSql;

use super::model::TickReport;
use super::tick;
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};

const POST: &str = "post";
const REMIND: &str = "remind";
const CLOSE: &str = "close_signups";
const ARCHIVE: &str = "archive";
const ROUTINE: &str = "routine";
const DIGEST: &str = "digest";
const FILL: &str = "fill_series";
const GOOGLE: &str = "google_sync";

/// How long a routine goes unchecked at most, in case time alone moved it on.
const ROUTINE_CHECK: i64 = 60;
/// How long a Google Calendar goes without a full pass at most.
const GOOGLE_CHECK: i64 = 6 * 60;

/// What a worker pass did.
pub struct Pass {
    pub report: TickReport,
    /// People whose Google Calendar is due a sync.
    pub google_users: Vec<i64>,
}

enum Step {
    Work(TickReport),
    Google(i64),
}

fn empty() -> TickReport {
    TickReport {
        notes: Vec::new(),
        jobs: Vec::new(),
        guilds: Vec::new(),
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

/// Runs up to `limit` due tasks, moving tasks for marks before each one.
/// Google syncs come back for the caller to push; without Google set up they
/// are dropped.
pub fn run_due(conn: &mut Connection, now: DateTime<Utc>, google: bool, limit: usize) -> Result<Pass, OpError> {
    let mut pass = Pass {
        report: empty(),
        google_users: Vec::new(),
    };
    if !google {
        drop_due_google(conn, now)?;
    }
    for _ in 0..limit {
        drain_marks(conn, now)?;
        match take_one(conn, now, google)? {
            None => break,
            Some(Step::Work(piece)) => merge(&mut pass.report, piece),
            Some(Step::Google(user_id)) => pass.google_users.push(user_id),
        }
    }
    drain_marks(conn, now)?;
    Ok(pass)
}

/// Everything due by `now`, until nothing is left: what `tick` does.
pub fn catch_up(conn: &mut Connection, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty();
    loop {
        let pass = run_due(conn, now, false, 500)?;
        let idle = pass.report.notes.is_empty() && pass.report.jobs.is_empty() && pass.report.guilds.is_empty();
        merge(&mut report, pass.report);
        if idle && next_wake(conn, false)?.is_none_or(|at| at > now) {
            return Ok(report);
        }
    }
}

/// When the earliest task is due.
pub fn next_wake(conn: &mut Connection, google: bool) -> Result<Option<DateTime<Utc>>, OpError> {
    Ok(schema::tasks::table
        .filter(schema::tasks::kind.ne(skipped(google)))
        .select(schema::tasks::run_at)
        .order(schema::tasks::run_at.asc())
        .first(conn)
        .optional()?)
}

/// The kind of task a worker without Google set up leaves alone; none otherwise.
fn skipped(google: bool) -> &'static str {
    if google { "" } else { GOOGLE }
}

/// Marks every connected Google Calendar for a pass, as the worker starts.
pub fn mark_all_google(conn: &mut Connection) -> Result<(), OpError> {
    diesel::insert_into(schema::task_marks::table)
        .values(schema::google_accounts::table.select(("google".into_sql::<Text>(), schema::google_accounts::user_id)))
        .into_columns((schema::task_marks::kind, schema::task_marks::subject_id))
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

/// After a Google pass: again at `again`, or the usual check later.
pub fn finish_google(conn: &mut Connection, user_id: i64, again: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Result<(), OpError> {
    if !google_connected(conn, user_id)? {
        return drop_task(conn, GOOGLE, user_id);
    }
    let check = now + Duration::minutes(GOOGLE_CHECK);
    put_soonest(conn, GOOGLE, user_id, again.map_or(check, |at| at.min(check)))
}

fn take_one(conn: &mut Connection, now: DateTime<Utc>, google: bool) -> Result<Option<Step>, OpError> {
    let claimed = store::with_tx(conn, |tx| {
        let Some((id, kind, subject)) = due(tx, now, google, true)? else {
            return Ok(None);
        };
        diesel::delete(schema::tasks::table.find(id)).execute(tx)?;
        if kind == GOOGLE {
            return Ok(Some(Step::Google(subject)));
        }
        let report = perform(tx, &kind, subject, now)?;
        Ok(Some(Step::Work(report)))
    });
    match claimed {
        Ok(step) => Ok(step),
        Err(err) => {
            // The task stays, a minute later, so one failure doesn't spin.
            if let Some((id, kind, subject)) = due(conn, now, google, false)? {
                diesel::update(schema::tasks::table.find(id))
                    .set(schema::tasks::run_at.eq(now + Duration::minutes(1)))
                    .execute(conn)?;
                tracing::warn!("task {kind} for {subject} failed, retrying in a minute: {err}");
            }
            Err(err)
        }
    }
}

/// The earliest task due by `now`, locked for this transaction when `lock`
/// so a second worker skips it.
fn due(conn: &mut Connection, now: DateTime<Utc>, google: bool, lock: bool) -> Result<Option<(i64, String, i64)>, OpError> {
    let query = schema::tasks::table
        .filter(schema::tasks::run_at.le(now))
        .filter(schema::tasks::kind.ne(skipped(google)))
        .order((schema::tasks::run_at.asc(), schema::tasks::id.asc()))
        .select((schema::tasks::id, schema::tasks::kind, schema::tasks::subject_id));
    Ok(if lock {
        query.for_update().skip_locked().first(conn).optional()?
    } else {
        query.first(conn).optional()?
    })
}

/// Does one task, then sets when its subject is next due.
fn perform(conn: &mut Connection, kind: &str, subject: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let report = match kind {
        POST => tick::post_one(conn, subject, now)?,
        REMIND => tick::remind_one(conn, subject, now)?,
        CLOSE => tick::close_one(conn, subject, now)?,
        ARCHIVE => tick::archive_one(conn, subject, now)?,
        ROUTINE => {
            let mut report = empty();
            if let Some(guild_id) = routine_guild(conn, subject)? {
                report.jobs = super::routine::kick(conn, subject, now)?;
                if !report.jobs.is_empty() {
                    report.guilds.push(guild_id);
                }
            }
            sync_routine_after(conn, subject, now, true)?;
            return Ok(report);
        }
        DIGEST => {
            let report = tick::digest_one(conn, subject, now)?;
            sync_clock(conn, subject, now)?;
            return Ok(report);
        }
        FILL => {
            tick::fill_guild(conn, subject, now)?;
            if active_series(conn, subject)? {
                put(conn, FILL, subject, now + Duration::hours(1))?;
            }
            return Ok(empty());
        }
        _ => return Ok(empty()),
    };
    // Whatever the task did, its event's other tasks follow from the event as it is now.
    let event_id = if kind == REMIND {
        schema::reminders::table
            .find(subject)
            .select(schema::reminders::event_id)
            .first(conn)
            .optional()?
    } else {
        Some(subject)
    };
    if let Some(event_id) = event_id {
        sync_event(conn, event_id, now)?;
    }
    Ok(report)
}

/// Turns what changed into task times.
fn drain_marks(conn: &mut Connection, now: DateTime<Utc>) -> Result<(), OpError> {
    store::with_tx(conn, |tx| {
        let marks: Vec<(String, i64)> = diesel::delete(schema::task_marks::table)
            .returning((schema::task_marks::kind, schema::task_marks::subject_id))
            .get_results(tx)?;
        if marks.is_empty() {
            return Ok(());
        }
        for (kind, subject) in &marks {
            let subject = *subject;
            match kind.as_str() {
                "event" => sync_event(tx, subject, now)?,
                "routine" => sync_routine(tx, subject, now)?,
                "guild" => sync_guild(tx, subject, now)?,
                "google" => note_google(tx, subject, now)?,
                "member" => {
                    let user: Option<i64> = schema::memberships::table
                        .find(subject)
                        .select(schema::memberships::user_id)
                        .first(tx)
                        .optional()?;
                    if let Some(user_id) = user {
                        note_google(tx, user_id, now)?;
                    }
                }
                "channel" => {
                    let guild: Option<i64> = schema::channels::table
                        .find(subject)
                        .select(schema::channels::guild_id)
                        .first(tx)
                        .optional()?;
                    if let Some(guild_id) = guild {
                        note_people(tx, guild_id, now)?;
                    }
                }
                "people" => note_people(tx, subject, now)?,
                _ => {}
            }
        }
        // A reminder that was deleted or sent has nothing left to do.
        diesel::delete(
            schema::tasks::table.filter(schema::tasks::kind.eq(REMIND)).filter(not(exists(
                schema::reminders::table
                    .filter(schema::reminders::id.eq(schema::tasks::subject_id))
                    .filter(schema::reminders::sent_at.is_null()),
            ))),
        )
        .execute(tx)?;
        Ok(())
    })
}

/// An event's post, reminders, signup cutoff and archive, from the event as it is.
pub fn sync_event(conn: &mut Connection, event_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let Some(event) = store::event(conn, event_id)? else {
        for kind in [POST, CLOSE, ARCHIVE] {
            drop_task(conn, kind, event_id)?;
        }
        return note_linked_google(conn, event_id, now);
    };
    match event.post_at.filter(|_| event.status == "scheduled" && event.post_attempts < 3) {
        Some(post_at) => put(conn, POST, event.id, post_at)?,
        None => drop_task(conn, POST, event.id)?,
    }
    let posted = event.status == "posted";
    for rem in store::reminders_for(conn, &event)? {
        if posted && rem.sent_at.is_none() {
            put(conn, REMIND, rem.id, event.starts_at - Duration::minutes(rem.offset_minutes))?;
        } else {
            drop_task(conn, REMIND, rem.id)?;
        }
    }
    match event.cutoff_at.filter(|_| posted && !event.signups_closed) {
        Some(cutoff) => put(conn, CLOSE, event.id, cutoff)?,
        None => drop_task(conn, CLOSE, event.id)?,
    }
    let settings = store::settings(conn, event.guild_id)?;
    match tick::archive_at(&event, &settings).filter(|_| posted) {
        Some(at) => put(conn, ARCHIVE, event.id, at)?,
        None => drop_task(conn, ARCHIVE, event.id)?,
    }
    // Whoever has the event in their calendar, or might now.
    if let Some(created_by) = event.created_by {
        if let Some(member) = store::membership(conn, created_by)? {
            note_google(conn, member.user_id, now)?;
        }
    }
    let signed: Vec<i64> = schema::signups::table
        .inner_join(schema::memberships::table)
        .filter(schema::signups::event_id.eq(event.id))
        .select(schema::memberships::user_id)
        .distinct()
        .load(conn)?;
    for user_id in signed {
        note_google(conn, user_id, now)?;
    }
    note_linked_google(conn, event.id, now)
}

/// When a routine next needs a look: a poll to open or lock, an approval to
/// lapse, or the hourly check that starts next week's poll.
pub fn sync_routine(conn: &mut Connection, routine_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    sync_routine_after(conn, routine_id, now, false)
}

/// `kicked`: the routine was just moved on, so with no poll on the way this
/// week's is done and the next starts when its window closes.
fn sync_routine_after(conn: &mut Connection, routine_id: i64, now: DateTime<Utc>, kicked: bool) -> Result<(), OpError> {
    let active: Option<bool> = schema::routines::table
        .find(routine_id)
        .select(schema::routines::active)
        .first(conn)
        .optional()?;
    let Some(active) = active else {
        return drop_task(conn, ROUTINE, routine_id);
    };
    let runs: Vec<(String, DateTime<Utc>, DateTime<Utc>, Option<DateTime<Utc>>)> = schema::routine_runs::table
        .filter(schema::routine_runs::routine_id.eq(routine_id))
        .select((
            schema::routine_runs::status,
            schema::routine_runs::opens_at,
            schema::routine_runs::locks_at,
            schema::routine_runs::decide_by,
        ))
        .load(conn)?;
    let mut next: Option<DateTime<Utc>> = None;
    let mut soonest = |at: DateTime<Utc>| next = Some(next.map_or(at, |next| next.min(at)));
    let mut current = false;
    for (status, opens, locks, decide_by) in &runs {
        match status.as_str() {
            "scheduled" => {
                current = true;
                soonest(*opens);
            }
            "open" => {
                current = true;
                soonest(*locks);
            }
            "approval" => {
                current = true;
                if let Some(at) = decide_by {
                    soonest(*at);
                }
            }
            _ => {}
        }
    }
    if active {
        let check = now + Duration::minutes(ROUTINE_CHECK);
        if current {
            soonest(check);
        } else if kicked {
            // This week's poll is settled; next week's starts as the window closes.
            soonest(super::routine::window_end(conn, routine_id, now)?.map_or(check, |end| end.min(check)));
        } else {
            soonest(now);
        }
    }
    match next {
        Some(at) => put(conn, ROUTINE, routine_id, at),
        None => drop_task(conn, ROUTINE, routine_id),
    }
}

/// A server's digest and series fills, and every event and routine in it.
pub fn sync_guild(conn: &mut Connection, guild_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    if store::guild(conn, guild_id)?.is_none() {
        drop_task(conn, DIGEST, guild_id)?;
        return drop_task(conn, FILL, guild_id);
    }
    sync_clock(conn, guild_id, now)?;
    let events: Vec<i64> = schema::events::table
        .filter(schema::events::guild_id.eq(guild_id))
        .filter(schema::events::status.eq_any(["posted", "scheduled"]))
        .select(schema::events::id)
        .load(conn)?;
    for event_id in events {
        sync_event(conn, event_id, now)?;
    }
    let routines: Vec<i64> = schema::routines::table
        .filter(schema::routines::guild_id.eq(guild_id))
        .select(schema::routines::id)
        .load(conn)?;
    for routine_id in routines {
        sync_routine(conn, routine_id, now)?;
    }
    note_people(conn, guild_id, now)
}

/// A server's digest, and the hourly fill while it has active series.
fn sync_clock(conn: &mut Connection, guild_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let settings = store::settings(conn, guild_id)?;
    match next_digest(conn, guild_id, &settings.digest, now)? {
        Some(at) => put(conn, DIGEST, guild_id, at)?,
        None => drop_task(conn, DIGEST, guild_id)?,
    }
    if active_series(conn, guild_id)? {
        // A new or restarted series fills now; otherwise the hourly fill stays.
        put_if_missing(conn, FILL, guild_id, now)
    } else {
        drop_task(conn, FILL, guild_id)
    }
}

/// When a server's next digest goes out: 9:00 in its zone on a digest day,
/// or now when today's is late and not yet sent.
fn next_digest(conn: &mut Connection, guild_id: i64, digest: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, OpError> {
    if digest != "daily" && digest != "weekly" {
        return Ok(None);
    }
    let guild = store::guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let today = now.with_timezone(&tz).date_naive();
    for offset in 0..8 {
        let day = today + Duration::days(offset);
        if !due_on(digest, day) {
            continue;
        }
        let at = nine_on(day, tz);
        if offset == 0 {
            if store::meta(conn, &tick::digest_key(guild_id, day))?.is_some() {
                continue;
            }
            return Ok(Some(at.max(now)));
        }
        return Ok(Some(at));
    }
    Ok(None)
}

fn due_on(digest: &str, day: NaiveDate) -> bool {
    digest == "daily" || (digest == "weekly" && day.weekday() == chrono::Weekday::Mon)
}

fn nine_on(day: NaiveDate, tz: Tz) -> DateTime<Utc> {
    day.and_hms_opt(9, 0, 0)
        .and_then(|local| tz.from_local_datetime(&local).earliest())
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&day.and_hms_opt(9, 0, 0).expect("9:00 exists")))
}

fn active_series(conn: &mut Connection, guild_id: i64) -> Result<bool, OpError> {
    let count: i64 = schema::series::table
        .filter(schema::series::guild_id.eq(guild_id))
        .filter(schema::series::active.eq(true))
        .count()
        .get_result(conn)?;
    Ok(count > 0)
}

fn routine_guild(conn: &mut Connection, routine_id: i64) -> Result<Option<i64>, OpError> {
    Ok(schema::routines::table
        .find(routine_id)
        .select(schema::routines::guild_id)
        .first(conn)
        .optional()?)
}

fn google_connected(conn: &mut Connection, user_id: i64) -> Result<bool, OpError> {
    let count: i64 = schema::google_accounts::table
        .filter(schema::google_accounts::user_id.eq(user_id))
        .count()
        .get_result(conn)?;
    Ok(count > 0)
}

/// A Google Calendar sync for `user_id` now, when they have one.
fn note_google(conn: &mut Connection, user_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    if google_connected(conn, user_id)? {
        put_soonest(conn, GOOGLE, user_id, now)?;
    }
    Ok(())
}

/// Everyone with an event in their calendar, so a removed event leaves it.
fn note_linked_google(conn: &mut Connection, event_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let users: Vec<i64> = schema::google_events::table
        .filter(schema::google_events::event_id.eq(event_id))
        .select(schema::google_events::user_id)
        .load(conn)?;
    for user_id in users {
        note_google(conn, user_id, now)?;
    }
    Ok(())
}

/// Everyone in a server with a calendar: who sees what there changed.
fn note_people(conn: &mut Connection, guild_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let users: Vec<i64> = schema::memberships::table
        .inner_join(schema::google_accounts::table.on(schema::google_accounts::user_id.eq(schema::memberships::user_id)))
        .filter(schema::memberships::guild_id.eq(guild_id))
        .select(schema::memberships::user_id)
        .distinct()
        .load(conn)?;
    for user_id in users {
        put_soonest(conn, GOOGLE, user_id, now)?;
    }
    Ok(())
}

fn drop_due_google(conn: &mut Connection, now: DateTime<Utc>) -> Result<(), OpError> {
    diesel::delete(
        schema::tasks::table
            .filter(schema::tasks::kind.eq(GOOGLE))
            .filter(schema::tasks::run_at.le(now)),
    )
    .execute(conn)?;
    Ok(())
}

fn put(conn: &mut Connection, kind: &str, subject: i64, at: DateTime<Utc>) -> Result<(), OpError> {
    diesel::insert_into(schema::tasks::table)
        .values(new_task(kind, subject, at))
        .on_conflict((schema::tasks::kind, schema::tasks::subject_id))
        .do_update()
        .set(schema::tasks::run_at.eq(excluded(schema::tasks::run_at)))
        .execute(conn)?;
    Ok(())
}

/// Like `put`, but a task already due sooner stays as it is.
fn put_soonest(conn: &mut Connection, kind: &str, subject: i64, at: DateTime<Utc>) -> Result<(), OpError> {
    diesel::insert_into(schema::tasks::table)
        .values(new_task(kind, subject, at))
        .on_conflict((schema::tasks::kind, schema::tasks::subject_id))
        .do_update()
        .set(schema::tasks::run_at.eq(schema::least(schema::tasks::run_at, excluded(schema::tasks::run_at))))
        .execute(conn)?;
    Ok(())
}

fn put_if_missing(conn: &mut Connection, kind: &str, subject: i64, at: DateTime<Utc>) -> Result<(), OpError> {
    diesel::insert_into(schema::tasks::table)
        .values(new_task(kind, subject, at))
        .on_conflict((schema::tasks::kind, schema::tasks::subject_id))
        .do_nothing()
        .execute(conn)?;
    Ok(())
}

fn new_task(
    kind: &str,
    subject: i64,
    at: DateTime<Utc>,
) -> (
    diesel::dsl::Eq<schema::tasks::kind, &str>,
    diesel::dsl::Eq<schema::tasks::subject_id, i64>,
    diesel::dsl::Eq<schema::tasks::run_at, DateTime<Utc>>,
) {
    (
        schema::tasks::kind.eq(kind),
        schema::tasks::subject_id.eq(subject),
        schema::tasks::run_at.eq(at),
    )
}

fn drop_task(conn: &mut Connection, kind: &str, subject: i64) -> Result<(), OpError> {
    diesel::delete(
        schema::tasks::table
            .filter(schema::tasks::kind.eq(kind))
            .filter(schema::tasks::subject_id.eq(subject)),
    )
    .execute(conn)?;
    Ok(())
}
