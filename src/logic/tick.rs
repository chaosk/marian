use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::{log, must_guild, must_membership};
use super::lifecycle::archive_tx;
use super::model::*;
use super::series::{fill_series, mark_posted};
use super::signup::cutoff_passed;
use crate::clock;
use crate::error::OpError;
use crate::jobs::DiscordJob;
use crate::schema;
use crate::store::{self, Connection, Event};

#[derive(Insertable)]
#[diesel(table_name = schema::thread_messages)]
struct NewThreadMessage<'a> {
    event_id: i64,
    body: &'a str,
    place: &'a str,
    created_at: DateTime<Utc>,
}

/// Brings everything up to `now`: moves the tasks for whatever changed, then
/// runs every task that is due, until nothing is left. The worker does the
/// same a step at a time; tests and catch-up use this.
pub fn tick(conn: &mut Connection, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    super::schedule::catch_up(conn, now)
}

fn empty_report() -> TickReport {
    TickReport {
        notes: Vec::new(),
        jobs: Vec::new(),
        guilds: Vec::new(),
    }
}

/// Posts a scheduled event whose time has come, or counts a failed try when
/// its channel is gone.
pub(super) fn post_one(conn: &mut Connection, event_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty_report();
    let Some(event) = store::event(conn, event_id)? else {
        return Ok(report);
    };
    if event.status != "scheduled" || event.post_attempts >= 3 || event.post_at.is_none_or(|at| at > now) {
        return Ok(report);
    }
    if store::channel(conn, event.channel_id)?.is_none() {
        fail_post(conn, &event)?;
        report.notes.push(format!("Post failed for {}", event.title));
    } else {
        mark_posted(conn, event.id, now)?;
        report.jobs.push(DiscordJob::sync(event.id));
        report.jobs.push(DiscordJob::attend(event.id));
        report.notes.push(format!("Posted {}", event.title));
    }
    report.guilds.push(event.guild_id);
    Ok(report)
}

/// Sends one reminder that is due, or notes that it came too late or had
/// nobody to ping.
pub(super) fn remind_one(conn: &mut Connection, reminder_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty_report();
    let Some((rem, event)) = schema::reminders::table
        .find(reminder_id)
        .inner_join(schema::events::table)
        .select((store::Reminder::as_select(), Event::as_select()))
        .first::<(store::Reminder, Event)>(conn)
        .optional()?
    else {
        return Ok(report);
    };
    if rem.sent_at.is_some() || event.status != "posted" {
        return Ok(report);
    }
    let start = event.starts_at;
    let due = start - Duration::minutes(rem.offset_minutes);
    if now < due {
        return Ok(report);
    }
    let guild_id = event.guild_id;
    report.guilds.push(guild_id);
    diesel::update(schema::reminders::table.filter(schema::reminders::id.eq(rem.id)))
        .set(schema::reminders::sent_at.eq(now))
        .execute(conn)?;
    if now > start + Duration::minutes(30) {
        let note = format!("Missed the reminder window for {}.", event.title);
        log(conn, guild_id, Some(event.id), &note)?;
        report.notes.push(note);
        return Ok(report);
    }
    let settings = store::settings(conn, guild_id)?;
    let posted = [event.clone()];
    let options = store::options_by_event(conn, &posted)?.remove(0);
    let signups = store::signups_by_event(conn, &posted)?.remove(0);
    let mut people = None;
    let cached = guild_people(conn, guild_id, &mut people)?;
    match reminder_text(conn, cached, &options, &signups)? {
        Some(text) => {
            let place = if event.thread_enabled && settings.thread_reminders {
                "thread"
            } else {
                "channel"
            };
            let mut body = format!(
                "{text}\nREMINDER: {} starts in {}.",
                event.title,
                clock::human_offset(rem.offset_minutes)
            );
            if !rem.message.trim().is_empty() {
                body.push('\n');
                body.push_str(rem.message.trim());
            }
            diesel::insert_into(schema::thread_messages::table)
                .values(&NewThreadMessage {
                    event_id: event.id,
                    body: &body,
                    place,
                    created_at: now,
                })
                .execute(conn)?;
            let note = format!(
                "Reminder sent for {}: {} before.",
                event.title,
                clock::human_offset(rem.offset_minutes)
            );
            log(conn, guild_id, Some(event.id), &note)?;
            report.jobs.push(DiscordJob::remind(event.id, text, rem.message.clone()));
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
    Ok(report)
}

/// Closes signups once an event's cutoff passes.
pub(super) fn close_one(conn: &mut Connection, event_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty_report();
    let Some(event) = store::event(conn, event_id)? else {
        return Ok(report);
    };
    if event.status != "posted" || event.signups_closed || !cutoff_passed(&event, now) {
        return Ok(report);
    }
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::signups_closed.eq(true),
            schema::events::updated_at.eq(now),
        ))
        .execute(conn)?;
    let note = format!("Signups for {} are closed.", event.title);
    log(conn, event.guild_id, Some(event.id), &note)?;
    report.notes.push(note);
    report.jobs.push(DiscordJob::sync(event.id));
    report.jobs.push(DiscordJob::attend(event.id));
    report.guilds.push(event.guild_id);
    Ok(report)
}

/// When a posted event is archived, if the server archives on its own.
pub(super) fn archive_at(event: &Event, settings: &store::Settings) -> Option<DateTime<Utc>> {
    settings.auto_archive.then(|| {
        event.starts_at + Duration::minutes(event.duration_minutes) + Duration::hours(settings.auto_archive_hours)
    })
}

/// Archives a posted event once it has ended and the server's wait has passed.
pub(super) fn archive_one(conn: &mut Connection, event_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty_report();
    let Some(event) = store::event(conn, event_id)? else {
        return Ok(report);
    };
    let settings = store::settings(conn, event.guild_id)?;
    if event.status != "posted" || archive_at(&event, &settings).is_none_or(|at| now < at) {
        return Ok(report);
    }
    archive_tx(conn, &event)?;
    report.jobs.push(DiscordJob::sync(event.id));
    report.jobs.push(DiscordJob::attend(event.id));
    report.notes.push(format!("Archived {}", event.title));
    report.guilds.push(event.guild_id);
    Ok(report)
}

/// Adds the next dates of a server's active series.
pub(super) fn fill_guild(conn: &mut Connection, guild_id: i64, now: DateTime<Utc>) -> Result<(), OpError> {
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let dates = grouped(store::events_in_active_series(conn, guild_id)?, |event| {
        event.series_id.unwrap_or(0)
    });
    for series in store::list_series(conn, guild_id)? {
        if series.active {
            let existing = dates.get(&series.id).map(Vec::as_slice).unwrap_or(&[]);
            fill_series(conn, &series, existing, tz, now)?;
        }
    }
    Ok(())
}

/// Whether a server's digest goes out on `local`'s day.
pub(super) fn digest_day(digest: &str, local: DateTime<Tz>) -> bool {
    digest == "daily" || (digest == "weekly" && local.weekday() == chrono::Weekday::Mon)
}

/// The day's digest, once a day from 9:00 in the server's zone.
pub(super) fn digest_one(conn: &mut Connection, guild_id: i64, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = empty_report();
    let settings = store::settings(conn, guild_id)?;
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone).unwrap_or(Tz::UTC);
    let local = now.with_timezone(&tz);
    if !digest_day(&settings.digest, local) || local.time().hour() < 9 {
        return Ok(report);
    }
    let key = digest_key(guild_id, local.date_naive());
    if store::meta(conn, &key)?.is_some() {
        return Ok(report);
    }
    let mut names = Vec::new();
    for event in store::list_events(conn, guild_id)? {
        if event.status != "posted" {
            continue;
        }
        let start = event.starts_at;
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
    report.guilds.push(guild_id);
    Ok(report)
}

/// The key that marks a server's digest for a day as sent.
pub(super) fn digest_key(guild_id: i64, day: chrono::NaiveDate) -> String {
    format!("digest:{guild_id}:{day}")
}

fn fail_post(conn: &mut Connection, event: &Event) -> Result<(), OpError> {
    let attempts = event.post_attempts + 1;
    let stamp = Utc::now();
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
    cache: &'a mut Option<HashMap<i64, store::Membership>>,
) -> Result<&'a HashMap<i64, store::Membership>, OpError> {
    if cache.is_none() {
        let loaded = store::list_memberships(conn, guild_id)?
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
    memberships: &HashMap<i64, store::Membership>,
    options: &[store::SignupOption],
    signups: &[store::Signup],
) -> Result<Option<String>, OpError> {
    let reminding: HashSet<i64> = options
        .iter()
        .filter(|opt| opt.reminders_enabled)
        .map(|opt| opt.id)
        .collect();
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();
    for signup in signups {
        if signup.state != "confirmed"
            || !reminding.contains(&signup.option_id)
            || !seen.insert(signup.membership_id)
        {
            continue;
        }
        let owned;
        let user = if let Some(user) = memberships.get(&signup.membership_id) {
            user
        } else {
            owned = must_membership(conn, signup.membership_id)?;
            &owned
        };
        if !user.reminders_enabled {
            continue;
        }
        let discord_id = store::discord_id_for_membership(conn, user.id)?;
        mentions.push(format!("<@{discord_id}>"));
    }
    if mentions.is_empty() {
        return Ok(None);
    }
    Ok(Some(mentions.join(" ")))
}

#[derive(Debug)]
pub struct ReminderCard {
    pub title: String,
    pub description: String,
    pub footer: String,
}

/// The Discord embed for a reminder. The title's relative time updates after
/// the message is sent. `link` is the event post, when one exists.
pub fn reminder_card(
    lang: crate::i18n::Lang,
    event_title: &str,
    start_unix: i64,
    end_unix: i64,
    link: Option<&str>,
    message: &str,
) -> ReminderCard {
    let label = event_title.replace(['[', ']'], "");
    let label = if label.trim().is_empty() {
        crate::tr!(lang, "post-event-fallback")
    } else {
        label
    };
    let name = match link.map(str::trim).filter(|value| !value.is_empty()) {
        Some(link) => format!("[{label}]({link})"),
        None => label,
    };
    let mut description = String::new();
    if !message.trim().is_empty() {
        description.push_str(message.trim());
        description.push_str("\n\n");
    }
    description.push_str(&name);
    description.push('\n');
    description.push_str(&format!("<t:{start_unix}:F> - <t:{end_unix}:t>"));
    ReminderCard {
        title: crate::tr!(lang, "reminder-title", start = format!("<t:{start_unix}:R>")),
        description,
        footer: crate::tr!(lang, "reminder-footer"),
    }
}

pub fn discord_message_link(guild_id: &str, channel_id: &str, message_id: &str) -> String {
    format!("https://discord.com/channels/{guild_id}/{channel_id}/{message_id}")
}
