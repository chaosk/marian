use chrono::{Duration, Utc};
use diesel::prelude::*;

use super::access::{can_delete, can_edit};
use super::db::{log, must_channel, must_event, must_series, must_membership};
use super::event::save_event;
use super::model::*;
use super::series;
use super::template;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event, Series, Settings, Membership};

pub fn archive_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't archive this event."));
        }
        archive_tx(tx, &event, &settings)
    })
}

pub(super) fn archive_tx(
    conn: &mut Connection,
    event: &Event,
    settings: &Settings,
) -> Result<String, OpError> {
    if event.status != "posted" {
        return Err(OpError::message("Only a posted event can be archived."));
    }
    let stamp = Utc::now();
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::status.eq("archived"),
            schema::events::archived_at.eq(&stamp),
            schema::events::updated_at.eq(&stamp),
        ))
        .execute(conn)?;
    let channel = must_channel(conn, event.channel_id)?;
    if settings.purge_channels && channel.kind == "event" {
        diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel.id)))
            .set(schema::channels::purged_at.eq(&stamp))
            .execute(conn)?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!("Cleared #{} after {}.", channel.name, event.title),
        )?;
    }
    let notice = format!(
        "Archived {}. The responses stay in the archive.",
        event.title
    );
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
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if !can_delete(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't delete this event."));
        }
        if matches!(event.status.as_str(), "draft" | "scheduled" | "failed") {
            let title = event.title.clone();
            diesel::delete(schema::events::table.filter(schema::events::id.eq(event.id)))
                .execute(tx)?;
            let notice = format!("Deleted {title}.");
            log(tx, event.guild_id, None, &notice)?;
            Ok(notice)
        } else if event.status == "posted" {
            let stamp = Utc::now();
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::status.eq("cancelled"),
                    schema::events::archived_at.eq(&stamp),
                    schema::events::updated_at.eq(&stamp),
                ))
                .execute(tx)?;
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
    let options = store::options_for(conn, &event)?;
    let reminders = store::reminders_for(conn, &event)?;
    let input = EventInput {
        id: None,
        guild_id: event.guild_id,
        actor_id,
        channel_id: event.channel_id,
        title: event.title.clone(),
        description: event.description.clone(),
        starts_at: event.starts_at + Duration::days(7),
        duration_minutes: event.duration_minutes,
        color: event.color.clone(),
        image_url: event.image_url.clone(),
        cutoff_at: None,
        attendee_role_id: event.attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, &event, "mention")?,
        restrict_role_ids: store::event_role_ids(conn, &event, "restrict")?,
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
    };
    save_event(conn, input)
}

pub fn skip_event(conn: &mut Connection, event_id: i64, actor_id: i64) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
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
        diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
            .set((
                schema::events::status.eq("skipped"),
                schema::events::updated_at.eq(Utc::now()),
            ))
            .execute(tx)?;
        let notice = format!("Skipped {}.", event.title);
        log(tx, event.guild_id, Some(event.id), &notice)?;
        Ok(notice)
    })
}

pub fn save_series_repeat(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
    repeat: RepeatSpec,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let series = must_series(tx, series_id)?;
        let actor = must_membership(tx, actor_id)?;
        may_change_series(tx, &series, &actor, "You can't change this series.")?;
        series::retune_series(tx, series_id, &repeat, Utc::now())?;
        let title = template::load(tx, series.template_id)
            .map(|template| template.title)
            .unwrap_or_else(|_| "Series".into());
        let notice = format!("Saved the {title} repeat.");
        log(tx, series.guild_id, None, &notice)?;
        Ok(notice)
    })
}

pub fn end_series(conn: &mut Connection, series_id: i64, actor_id: i64) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let series = must_series(tx, series_id)?;
        let actor = must_membership(tx, actor_id)?;
        may_change_series(tx, &series, &actor, "You can't end this series.")?;
        diesel::update(schema::series::table.filter(schema::series::id.eq(series_id)))
            .set(schema::series::active.eq(false))
            .execute(tx)?;
        diesel::update(
            schema::events::table
                .filter(schema::events::series_id.eq(series_id))
                .filter(schema::events::status.eq_any(["scheduled", "draft"])),
        )
        .set((
            schema::events::status.eq("skipped"),
            schema::events::updated_at.eq(Utc::now()),
        ))
        .execute(tx)?;
        let title = super::template::load(tx, series.template_id)
            .map(|template| template.title)
            .unwrap_or_else(|_| "Series".into());
        let notice = format!("Ended the {title} series. Upcoming dates were skipped.");
        log(tx, series.guild_id, None, &notice)?;
        Ok(notice)
    })
}

fn may_change_series(
    conn: &mut Connection,
    series: &Series,
    actor: &Membership,
    denied: &str,
) -> Result<(), OpError> {
    if actor.guild_id != series.guild_id {
        return Err(OpError::NotFound);
    }
    if actor.organizer {
        return Ok(());
    }
    let settings = store::settings(conn, series.guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
    let events = store::events_in_series(conn, series.id)?;
    if events
        .iter()
        .any(|event| can_edit(actor, event, &settings, &roles))
    {
        Ok(())
    } else {
        Err(OpError::message(denied))
    }
}
