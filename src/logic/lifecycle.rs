use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

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

pub async fn archive_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, async |tx| {
        let event = must_event(tx, event_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        let settings = store::settings(tx, event.guild_id).await?;
        let roles = store::membership_role_ids(tx, &actor).await?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-cant-archive")));
        }
        archive_tx(tx, &event).await
    })
    .await
}

pub(super) async fn archive_tx(conn: &mut Connection, event: &Event) -> Result<String, OpError> {
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
        .execute(conn)
        .await?;
    let notice = format!(
        "Archived {}. The responses stay in the archive.",
        event.title
    );
    log(conn, event.guild_id, Some(event.id), &notice).await?;
    Ok(notice)
}

pub async fn delete_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<crate::i18n::Msg, OpError> {
    store::with_tx(conn, async |tx| {
        let event = must_event(tx, event_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        let settings = store::settings(tx, event.guild_id).await?;
        let roles = store::membership_role_ids(tx, &actor).await?;
        if !can_delete(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-cant-delete")));
        }
        if matches!(
            event.status,
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
        ) && series_keeps_its_dates(tx, &event).await?
        {
            // Deleting the row would leave a gap the series fills straight back in,
            // so a date of a series stays behind as skipped.
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::status.eq(EventStatus::Skipped),
                    schema::events::updated_at.eq(Utc::now()),
                ))
                .execute(tx)
                .await?;
            let notice = crate::msg!("notice-deleted-series-date", title = &event.title);
            log(tx, event.guild_id, Some(event.id), &notice.to_string()).await?;
            Ok(notice)
        } else if matches!(
            event.status,
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed
        ) {
            let title = event.title.clone();
            diesel::delete(schema::events::table.filter(schema::events::id.eq(event.id)))
                .execute(tx)
                .await?;
            let notice = crate::msg!("notice-deleted", title = title);
            log(tx, event.guild_id, None, &notice.to_string()).await?;
            Ok(notice)
        } else if event.status == EventStatus::Posted {
            let stamp = Utc::now();
            diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
                .set((
                    schema::events::status.eq(EventStatus::Cancelled),
                    schema::events::archived_at.eq(&stamp),
                    schema::events::updated_at.eq(&stamp),
                ))
                .execute(tx)
                .await?;
            let notice = crate::msg!("notice-cancelled", title = &event.title);
            log(tx, event.guild_id, Some(event.id), &notice.to_string()).await?;
            Ok(notice)
        } else {
            Err(OpError::from(crate::msg!("err-event-closed")))
        }
    })
    .await
}

/// Whether the event is a date of a series that has other dates, which would refill
/// the date if its row went away.
async fn series_keeps_its_dates(conn: &mut Connection, event: &Event) -> Result<bool, OpError> {
    let Some(series_id) = event.series_id else {
        return Ok(false);
    };
    Ok(store::events_in_series(conn, series_id)
        .await?
        .iter()
        .any(|date| date.id != event.id))
}

pub async fn duplicate_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<i64, OpError> {
    let event = must_event(conn, event_id).await?;
    let options = store::options_for(conn, &event).await?;
    let reminders = store::reminders_for(conn, &event).await?;
    // A week later at the same time on the clock, even across a daylight saving change.
    let zone = store::guild(conn, event.guild_id)
        .await?
        .map(|guild| guild.timezone)
        .unwrap_or_else(|| "UTC".into());
    let starts_at = crate::clock::occurrence_start(
        event.starts_at,
        2,
        Cadence::Weekly,
        crate::clock::zone(&zone)?,
    );
    // The copy keeps one of the server's roles only for someone who may pick it.
    let actor = must_membership(conn, actor_id).await?;
    let attendee_role_id = match event.attendee_role_id {
        Some(role_id) if may_pick_attendee_role(&actor, &must_role(conn, role_id).await?) => {
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
        mention_role_ids: store::event_role_ids(conn, &event, EventRoleKind::Mention).await?,
        restrict_role_ids: store::event_role_ids(conn, &event, EventRoleKind::Restrict).await?,
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
    save_event(conn, input).await
}

pub async fn skip_event(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, async |tx| {
        let event = must_event(tx, event_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        let settings = store::settings(tx, event.guild_id).await?;
        let roles = store::membership_role_ids(tx, &actor).await?;
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
            .execute(tx)
            .await?;
        let notice = format!("Skipped {}.", event.title);
        log(tx, event.guild_id, Some(event.id), &notice).await?;
        Ok(notice)
    })
    .await
}

pub async fn save_series_repeat(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
    repeat: RepeatSpec,
) -> Result<String, OpError> {
    store::with_tx(conn, async |tx| {
        let series = must_series(tx, series_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        may_change_series(tx, &series, &actor, crate::msg!("err-cant-change-series")).await?;
        series::retune_series(tx, series_id, &repeat, Utc::now()).await?;
        let title = template::load(tx, series.template_id)
            .await
            .map(|template| template.title)
            .unwrap_or_else(|_| "Series".into());
        let notice = format!("Saved the {title} repeat.");
        log(tx, series.guild_id, None, &notice).await?;
        Ok(notice)
    })
    .await
}

pub async fn end_series(
    conn: &mut Connection,
    series_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, async |tx| {
        let series = must_series(tx, series_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        may_change_series(tx, &series, &actor, crate::msg!("err-cant-end-series")).await?;
        diesel::update(schema::series::table.filter(schema::series::id.eq(series_id)))
            .set(schema::series::active.eq(false))
            .execute(tx)
            .await?;
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
        .execute(tx)
        .await?;
        let title = super::template::load(tx, series.template_id)
            .await
            .map(|template| template.title)
            .unwrap_or_else(|_| "Series".into());
        let notice = format!("Ended the {title} series. Upcoming dates were skipped.");
        log(tx, series.guild_id, None, &notice).await?;
        Ok(notice)
    })
    .await
}

async fn may_change_series(
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
    let settings = store::settings(conn, series.guild_id).await?;
    let roles = store::membership_role_ids(conn, actor).await?;
    let events = store::events_in_series(conn, series.id).await?;
    if events
        .iter()
        .any(|event| can_edit(actor, event, &settings, &roles))
    {
        Ok(())
    } else {
        Err(OpError::from(denied))
    }
}
