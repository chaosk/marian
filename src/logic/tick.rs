use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::{log, must_event, must_guild, must_membership};
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

pub fn tick(conn: &mut Connection, now: DateTime<Utc>) -> Result<TickReport, OpError> {
    let mut report = TickReport {
        notes: Vec::new(),
        jobs: Vec::new(),
        guilds: Vec::new(),
    };
    let guilds = store::list_guilds(conn)?;
    for guild in guilds {
        let piece = store::with_tx(conn, |tx| tick_guild(tx, guild.id, now))?;
        if !piece.notes.is_empty() || !piece.jobs.is_empty() {
            report.guilds.push(guild.id);
        }
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
        guilds: Vec::new(),
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
        let Some(post_at) = event.post_at else {
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

    let mut people = None;
    let events = store::list_events(conn, guild_id)?;
    let posted: Vec<Event> = events
        .iter()
        .filter(|event| event.status == "posted")
        .cloned()
        .collect();
    let reminder_groups = store::reminders_by_event(conn, &posted)?;
    let option_groups = store::options_by_event(conn, &posted)?;
    let signup_groups = store::signups_by_event(conn, &posted)?;
    for (index, event) in posted.iter().enumerate() {
        let start = event.starts_at;
        for rem in &reminder_groups[index] {
            if rem.sent_at.is_some() {
                continue;
            }
            let due = start - Duration::minutes(rem.offset_minutes);
            if now < due {
                continue;
            }
            diesel::update(schema::reminders::table.filter(schema::reminders::id.eq(rem.id)))
                .set(schema::reminders::sent_at.eq(now))
                .execute(conn)?;
            if now > start + Duration::minutes(30) {
                let note = format!("Missed the reminder window for {}.", event.title);
                log(conn, guild_id, Some(event.id), &note)?;
                report.notes.push(note);
                continue;
            }
            let cached = guild_people(conn, guild_id, &mut people)?;
            let text = reminder_text(
                conn,
                cached,
                &option_groups[index],
                &signup_groups[index],
            )?;
            match text {
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
                    report.jobs.push(DiscordJob::remind(
                        event.id,
                        text,
                        rem.message.clone(),
                    ));
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
                    schema::events::updated_at.eq(now),
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
                    archive_tx(conn, &fresh)?;
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
            }
        }
    }
    Ok(report)
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
