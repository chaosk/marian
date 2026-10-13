use chrono::Duration;
use rusqlite::{params, Connection};

use super::access::{can_delete, can_edit};
use super::db::{log, must_channel, must_event, must_series, must_user};
use super::event::save_event;
use super::model::*;
use crate::clock;
use crate::error::OpError;
use crate::store::{self, Event, Settings};

pub fn archive_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_user(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::user_role_ids(tx, actor.id)?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't archive this event."));
        }
        archive_tx(tx, &event, &settings)
    })
}

pub(super) fn archive_tx(
    conn: &Connection,
    event: &Event,
    settings: &Settings,
) -> Result<String, OpError> {
    if event.status != "posted" {
        return Err(OpError::message("Only a posted event can be archived."));
    }
    let stamp = store::now_stamp();
    conn.execute(
        "UPDATE events SET status = 'archived', archived_at = ?, updated_at = ? WHERE id = ?",
        params![stamp, stamp, event.id],
    )?;
    let channel = must_channel(conn, event.channel_id)?;
    if settings.purge_channels && channel.kind == "event" {
        conn.execute(
            "UPDATE channels SET purged_at = ? WHERE id = ?",
            params![stamp, channel.id],
        )?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!("Cleared #{} after {}.", channel.name, event.title),
        )?;
    }
    let notice = format!("Archived {}. The roster stays in the archive.", event.title);
    log(conn, event.guild_id, Some(event.id), &notice)?;
    Ok(notice)
}

pub fn delete_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_user(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::user_role_ids(tx, actor.id)?;
        if !can_delete(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't delete this event."));
        }
        if matches!(event.status.as_str(), "draft" | "scheduled" | "failed") {
            let title = event.title.clone();
            tx.execute("DELETE FROM events WHERE id = ?", [event.id])?;
            let notice = format!("Deleted {title}.");
            log(tx, event.guild_id, None, &notice)?;
            Ok(notice)
        } else if event.status == "posted" {
            let stamp = store::now_stamp();
            tx.execute(
                "UPDATE events SET status = 'cancelled', archived_at = ?, updated_at = ? WHERE id = ?",
                params![stamp, stamp, event.id],
            )?;
            let notice = format!("Cancelled {}.", event.title);
            log(tx, event.guild_id, Some(event.id), &notice)?;
            Ok(notice)
        } else {
            Err(OpError::message("That event is already closed."))
        }
    })
}

pub fn duplicate_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<i64, OpError> {
    let event = must_event(conn, event_id)?;
    let options = store::options_for(conn, event_id)?;
    let reminders = store::reminders_for(conn, event_id)?;
    let input = EventInput {
        id: None,
        guild_id: event.guild_id,
        actor_id,
        channel_id: event.channel_id,
        title: event.title.clone(),
        description: event.description.clone(),
        starts_at: clock::parse_stamp(&event.starts_at)? + Duration::days(7),
        duration_minutes: event.duration_minutes,
        color: event.color.clone(),
        image_url: event.image_url.clone(),
        cutoff_at: None,
        attendee_role_id: event.attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, event_id, "mention")?,
        restrict_role_ids: store::event_role_ids(conn, event_id, "restrict")?,
        options: options
            .into_iter()
            .map(|opt| OptionInput {
                id: None,
                name: opt.name,
                emoji: opt.emoji,
                capacity: opt.capacity,
                waitlist: opt.waitlist,
                role_id: opt.role_id,
                reminders_enabled: opt.reminders_enabled,
            })
            .collect(),
        reminders: reminders
            .into_iter()
            .map(|rem| ReminderInput {
                offset_minutes: rem.offset_minutes,
                message: rem.message,
            })
            .collect(),
        repeat: None,
        action: SaveAction::Draft,
        post_at: None,
        apply_series: false,
        vote: None,
    };
    save_event(conn, input)
}

pub fn skip_event(conn: &mut Connection, event_id: i64, actor_id: i64) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_user(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::user_role_ids(tx, actor.id)?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't skip this date."));
        }
        if event.series_id.is_none() {
            return Err(OpError::message("Only a date in a series can be skipped."));
        }
        if !matches!(event.status.as_str(), "scheduled" | "draft") {
            return Err(OpError::message(
                "Only a date that hasn't been posted can be skipped.",
            ));
        }
        tx.execute(
            "UPDATE events SET status = 'skipped', updated_at = ? WHERE id = ?",
            params![store::now_stamp(), event.id],
        )?;
        let notice = format!("Skipped {}.", event.title);
        log(tx, event.guild_id, Some(event.id), &notice)?;
        Ok(notice)
    })
}

pub fn end_series(conn: &mut Connection, series_id: i64, actor_id: i64) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let series = must_series(tx, series_id)?;
        let actor = must_user(tx, actor_id)?;
        if !actor.organizer && actor.guild_id == series.guild_id {
            let settings = store::settings(tx, series.guild_id)?;
            let roles = store::user_role_ids(tx, actor.id)?;
            let events = store::events_in_series(tx, series_id)?;
            let allowed = events
                .iter()
                .any(|event| can_edit(&actor, event, &settings, &roles));
            if !allowed {
                return Err(OpError::message("You can't end this series."));
            }
        }
        if actor.guild_id != series.guild_id {
            return Err(OpError::NotFound);
        }
        tx.execute("UPDATE series SET active = 0 WHERE id = ?", [series_id])?;
        tx.execute(
            "UPDATE rounds SET status = 'closed' WHERE series_id = ? AND status = 'open'",
            [series_id],
        )?;
        tx.execute(
            "UPDATE events SET status = 'skipped', updated_at = ? WHERE series_id = ? AND status IN ('scheduled', 'draft', 'ballot')",
            params![store::now_stamp(), series_id],
        )?;
        let template: SeriesTemplate =
            serde_json::from_str(&series.template_json).unwrap_or(SeriesTemplate {
                title: "Series".into(),
                description: String::new(),
                duration_minutes: 60,
                channel_id: 0,
                color: "#1e3a6e".into(),
                image_url: None,
                thread_enabled: false,
                multiple_signups: false,
                mention_everyone: false,
                attendee_role_id: None,
                mention_role_ids: vec![],
                restrict_role_ids: vec![],
                cutoff_offset_minutes: None,
                options: vec![],
                reminders: vec![],
                vote: VoteSpec::default(),
            });
        let notice = format!(
            "Ended the {} series. Upcoming dates were skipped.",
            template.title
        );
        log(tx, series.guild_id, None, &notice)?;
        Ok(notice)
    })
}
