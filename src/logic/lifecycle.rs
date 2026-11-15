use chrono::Utc;
use diesel::prelude::*;

use super::access::{can_delete, can_edit, may_pick_attendee_role};
use super::db::{log, must_event, must_membership, must_role, must_series};
use super::event::save_event;
use super::model::*;
use super::series;
use super::template;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event, Membership, Series};
use crate::values::{Cadence, EventRoleKind, EventStatus};

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
            return Err(OpError::from(crate::msg!("err-cant-archive")));
        }
        archive_tx(tx, &event)
    })
}

pub(super) fn archive_tx(conn: &mut Connection, event: &Event) -> Result<String, OpError> {
    if event.status != EventStatus::Posted {
        return Err(OpError::from(crate::msg!("err-archive-unposted")));
    }
    let stamp = Utc::now();
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set((
            schema::events::status.eq(EventStatus::Archived),
            schema::events::archived_at.eq(&stamp),
            schema::events::updated_at.eq(&stamp),
        ))
        .execute(conn)?;
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
) -> Result<crate::i18n::Msg, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_membership(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::membership_role_ids(tx, &actor)?;
        if !can_delete(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-cant-delete")));
        }
        if matches!(
            event.status,
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
        ) && series_keeps_its_dates(tx, &event)?
        {
            // Deleting the row would leave a gap the series fills straight back in,
            // so a date of a series stays behind as skipped.
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::status.eq(EventStatus::Skipped),
                    schema::events::updated_at.eq(Utc::now()),
                ))
                .execute(tx)?;
            let notice = crate::msg!("notice-deleted-series-date", title = &event.title);
            log(tx, event.guild_id, Some(event.id), &notice.to_string())?;
            Ok(notice)
        } else if matches!(
            event.status,
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
        ) {
            let title = event.title.clone();
            diesel::delete(schema::events::table.filter(schema::events::id.eq(event.id)))
                .execute(tx)?;
            let notice = crate::msg!("notice-deleted", title = title);
            log(tx, event.guild_id, None, &notice.to_string())?;
            Ok(notice)
        } else if event.status == EventStatus::Posted {
            let stamp = Utc::now();
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::status.eq(EventStatus::Cancelled),
                    schema::events::archived_at.eq(&stamp),
                    schema::events::updated_at.eq(&stamp),
                ))
                .execute(tx)?;
            let notice = crate::msg!("notice-cancelled", title = &event.title);
            log(tx, event.guild_id, Some(event.id), &notice.to_string())?;
            Ok(notice)
        } else {
            Err(OpError::from(crate::msg!("err-event-closed")))
        }
    })
}

/// Whether the event is a date of a series that has other dates, which would refill
/// the date if its row went away.
fn series_keeps_its_dates(conn: &mut Connection, event: &Event) -> Result<bool, OpError> {
    let Some(series_id) = event.series_id else {
        return Ok(false);
    };
    Ok(store::events_in_series(conn, series_id)?
        .iter()
        .any(|date| date.id != event.id))
}

pub fn duplicate_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<i64, OpError> {
    let event = must_event(conn, event_id)?;
    let options = store::options_for(conn, &event)?;
    let reminders = store::reminders_for(conn, &event)?;
    // A week later at the same time on the clock, even across a daylight saving change.
    let zone = store::guild(conn, event.guild_id)?
        .map(|guild| guild.timezone)
        .unwrap_or_else(|| "UTC".into());
    let starts_at = crate::clock::occurrence_start(
        event.starts_at,
        2,
        Cadence::Weekly,
        crate::clock::zone(&zone)?,
    );
    // The copy keeps one of the server's roles only for someone who may pick it.
    let actor = must_membership(conn, actor_id)?;
    let attendee_role_id = match event.attendee_role_id {
        Some(role_id) if may_pick_attendee_role(&actor, &must_role(conn, role_id)?) => {
            Some(role_id)
        }
        _ => None,
    };
    let input = EventInput {
        id: None,
        guild_id: event.guild_id,
        actor_id,
        channel_id: event.channel_id,
        title: event.title.clone(),
        description: event.description.clone(),
        location: event.location.clone(),
        starts_at,
        duration_minutes: event.duration_minutes,
        color: event.color.clone(),
        image_url: event.image_url.clone(),
        cutoff_at: None,
        attendee_role_id,
        thread_enabled: event.thread_enabled,
        multiple_signups: event.multiple_signups,
        mention_everyone: event.mention_everyone,
        mention_role_ids: store::event_role_ids(conn, &event, EventRoleKind::Mention)?,
        restrict_role_ids: store::event_role_ids(conn, &event, EventRoleKind::Restrict)?,
        options: options
            .into_iter()
            .map(|opt| OptionInput {
                id: None,
                type_id: opt.type_id,
                capacity: opt.capacity,
                max_guests: opt.max_guests,
                role_id: opt.role_id,
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
            return Err(OpError::from(crate::msg!("err-cant-skip")));
        }
        if event.series_id.is_none() {
            return Err(OpError::from(crate::msg!("err-skip-not-series")));
        }
        if !matches!(event.status, EventStatus::Scheduled | EventStatus::Draft) {
            return Err(OpError::from(crate::msg!("err-skip-posted")));
        }
        diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
            .set((
                schema::events::status.eq(EventStatus::Skipped),
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
        may_change_series(tx, &series, &actor, crate::msg!("err-cant-change-series"))?;
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
        may_change_series(tx, &series, &actor, crate::msg!("err-cant-end-series"))?;
        diesel::update(schema::series::table.filter(schema::series::id.eq(series_id)))
            .set(schema::series::active.eq(false))
            .execute(tx)?;
        diesel::update(
            schema::events::table
                .filter(schema::events::series_id.eq(series_id))
                .filter(
                    schema::events::status.eq_any([EventStatus::Scheduled, EventStatus::Draft]),
                ),
        )
        .set((
            schema::events::status.eq(EventStatus::Skipped),
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
    denied: crate::i18n::Msg,
) -> Result<(), OpError> {
    if actor.guild_id != series.guild_id {
        return Err(OpError::NotFound);
    }
    if actor.server_manager {
        return Ok(());
    }
    let settings = store::settings(conn, series.guild_id)?;
    let roles = store::membership_role_ids(conn, actor)?;
    let events = store::events_in_series(conn, series.id)?;
    if events
        .iter()
        .any(|event| can_edit(actor, event, &settings, &roles))
    {
        Ok(())
    } else {
        Err(OpError::from(denied))
    }
}
