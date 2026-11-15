use std::collections::HashSet;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::access::{can_create, can_edit, may_pick_attendee_role};
use super::db::{log, must_channel, must_event, must_membership, must_role};
use super::library::clean_types;
use super::model::*;
use super::series::{apply_series_template, ensure_series, link_new_series, mark_posted};
use super::signup::{fill_all, sync_access};
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};
use crate::values::{Cadence, EventRoleKind, EventStatus};

pub async fn save_event(conn: &mut Connection, input: EventInput) -> Result<i64, OpError> {
    let clean = normalize(conn, &input).await?;
    let now = Utc::now();
    store::with_tx(conn, async |tx| {
        let id = if let Some(existing) = &clean.existing {
            update_event_row(tx, existing.id, &input, &clean, now).await?;
            sync_options(tx, existing.id, &clean.options).await?;
            if existing.status == EventStatus::Posted {
                replace_reminders(tx, existing, &clean.reminders).await?;
            } else {
                diesel::delete(
                    schema::reminders::table.filter(schema::reminders::event_id.eq(existing.id)),
                )
                .execute(tx)
                .await?;
                insert_reminders(tx, existing.id, &clean.reminders, &[]).await?;
            }
            replace_roles(tx, existing.id, &input).await?;
            existing.id
        } else {
            let id = insert_event_row(tx, &input, &clean, now).await?;
            replace_children(tx, id, &clean.options, &clean.reminders).await?;
            replace_roles(tx, id, &input).await?;
            id
        };

        transition(tx, id, &input, now).await?;

        let event = must_event(tx, id).await?;
        if input.repeat.is_some()
            && event.series_id.is_none()
            && clean.existing.as_ref().and_then(|e| e.series_id).is_none()
        {
            link_new_series(tx, &event, &input, &clean, now).await?;
        }
        let event = must_event(tx, id).await?;
        if input.apply_series {
            if let Some(series_id) = event.series_id {
                apply_series_template(tx, series_id, id, &input, &clean).await?;
            }
        }
        let event = must_event(tx, id).await?;
        if let Some(series_id) = event.series_id {
            if matches!(
                event.status,
                EventStatus::Posted | EventStatus::Scheduled | EventStatus::Archived
            ) {
                ensure_series(tx, series_id, now).await?;
            }
        }
        if event.status == EventStatus::Posted {
            fill_all(tx, &event).await?;
            sync_access(tx, id).await?;
        }
        Ok(id)
    })
    .await
}

async fn normalize(conn: &mut Connection, input: &EventInput) -> Result<CleanInput, OpError> {
    let settings = store::settings(conn, input.guild_id).await?;
    let actor = must_membership(conn, input.actor_id).await?;
    if actor.guild_id != input.guild_id {
        return Err(OpError::from(crate::msg!("err-person-elsewhere")));
    }
    let roles = store::membership_role_ids(conn, &actor).await?;
    let existing = if let Some(id) = input.id {
        let event = must_event(conn, id).await?;
        if event.guild_id != input.guild_id {
            return Err(OpError::NotFound);
        }
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-event-not-yours")));
        }
        if matches!(
            event.status,
            EventStatus::Archived | EventStatus::Skipped | EventStatus::Cancelled
        ) {
            return Err(OpError::from(crate::msg!("err-date-closed")));
        }
        Some(event)
    } else {
        if !can_create(&actor, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-no-event-role")));
        }
        None
    };

    let title = input.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(OpError::from(crate::msg!("err-title-length")));
    }
    let description = input.description.trim().to_string();
    if description.chars().count() > 2000 {
        return Err(OpError::from(crate::msg!("err-description-length")));
    }
    let location = input.location.trim().to_string();
    if location.chars().count() > 200 {
        return Err(OpError::from(crate::msg!("err-location-length")));
    }
    if !(15..=1440).contains(&input.duration_minutes) {
        return Err(OpError::from(crate::msg!("err-duration")));
    }
    if !valid_color(&input.color) {
        return Err(OpError::from(crate::msg!("err-color")));
    }
    let image_url = match input
        .image_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(url)
            if (url.starts_with("https://") || url.starts_with("http://")) && url.len() <= 500 =>
        {
            Some(url.to_string())
        }
        Some(_) => return Err(OpError::from(crate::msg!("err-image-link"))),
    };
    let channel = must_channel(conn, input.channel_id).await?;
    if channel.guild_id != input.guild_id {
        return Err(OpError::from(crate::msg!("err-channel-elsewhere")));
    }
    let mut attendee_role_by = None;
    if let Some(role_id) = input.attendee_role_id {
        let role = must_role(conn, role_id).await?;
        if role.guild_id != input.guild_id {
            return Err(OpError::from(crate::msg!("err-attendee-role-elsewhere")));
        }
        // An event keeps the role it has, whoever saves it. Picking another
        // one of the server's roles takes a server manager.
        match existing
            .as_ref()
            .filter(|event| event.attendee_role_id == Some(role_id))
        {
            Some(event) => attendee_role_by = event.attendee_role_by,
            None if may_pick_attendee_role(&actor, &role) => attendee_role_by = Some(actor.id),
            None => return Err(OpError::from(crate::msg!("err-attendee-role-managers"))),
        }
    }
    for role_id in input
        .mention_role_ids
        .iter()
        .chain(input.restrict_role_ids.iter())
    {
        let role = must_role(conn, *role_id).await?;
        if role.guild_id != input.guild_id {
            return Err(OpError::from(crate::msg!("err-selected-role-elsewhere")));
        }
    }
    if let Some(cutoff) = input.cutoff_at {
        if cutoff > input.starts_at {
            return Err(OpError::from(crate::msg!("err-cutoff-late")));
        }
    }

    let options = clean_types(conn, input.guild_id, &input.options).await?;
    if let Some(existing) = &existing {
        known_options(conn, existing, &options).await?;
    }

    let mut reminders = clean_reminders(&input.reminders)?;
    // An event that goes out gets the server's reminder when it has none. Later
    // saves keep an empty list empty, so reminders can be cleared.
    // Zero minutes turns the server's default reminder off.
    if reminders.is_empty()
        && settings.default_reminder_minutes > 0
        && matches!(input.action, SaveAction::Publish | SaveAction::Schedule)
    {
        reminders.push(ReminderInput {
            offset_minutes: settings.default_reminder_minutes.clamp(1, 10_080),
            message: String::new(),
        });
    }
    if reminders.len() > 5 {
        return Err(OpError::from(crate::msg!("err-reminders-many")));
    }

    if let Some(repeat) = &input.repeat {
        if repeat.cadence == Cadence::Once {
            return Err(OpError::from(crate::msg!("err-repeat-values")));
        }
        if let Some(max) = repeat.max_occurrences {
            if max < 2 {
                return Err(OpError::from(crate::msg!("err-series-two-dates")));
            }
        }
        if let Some(end) = repeat.end_on {
            if end < input.starts_at {
                return Err(OpError::from(crate::msg!("err-series-end-early")));
            }
        }
    }
    if matches!(input.action, SaveAction::Schedule) {
        let post_at = input
            .post_at
            .ok_or_else(|| OpError::from(crate::msg!("err-post-time")))?;
        if post_at <= Utc::now() {
            return Err(OpError::from(crate::msg!("err-post-time-past")));
        }
        if post_at >= input.starts_at {
            return Err(OpError::from(crate::msg!("err-post-before-start")));
        }
    }

    Ok(CleanInput {
        title,
        description,
        location,
        image_url,
        options,
        reminders,
        existing,
        attendee_role_by,
    })
}

/// An option that names an id has to be one of this event's.
async fn known_options(
    conn: &mut Connection,
    event: &store::Event,
    options: &[OptionInput],
) -> Result<(), OpError> {
    let current: HashSet<i64> = store::options_for(conn, event)
        .await?
        .iter()
        .map(|opt| opt.id)
        .collect();
    let mut seen = HashSet::new();
    for id in options.iter().filter_map(|opt| opt.id) {
        if !current.contains(&id) || !seen.insert(id) {
            return Err(OpError::NotFound);
        }
    }
    Ok(())
}

/// Changes an event's options in their new order. Options with an id are
/// updated and keep their responses, options without one are added, and options
/// left out go, with any responses on them.
pub async fn save_options(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
    input: &[OptionInput],
) -> Result<(), OpError> {
    store::with_tx(conn, async |tx| {
        let event = must_event(tx, event_id).await?;
        let actor = must_membership(tx, actor_id).await?;
        let settings = store::settings(tx, event.guild_id).await?;
        let roles = store::membership_role_ids(tx, &actor).await?;
        if actor.guild_id != event.guild_id || !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::from(crate::msg!("err-event-not-yours")));
        }
        if matches!(
            event.status,
            EventStatus::Archived | EventStatus::Skipped | EventStatus::Cancelled
        ) {
            return Err(OpError::from(crate::msg!("err-date-closed")));
        }
        let options = clean_types(tx, event.guild_id, input).await?;
        known_options(tx, &event, &options).await?;
        sync_options(tx, event.id, &options).await?;
        if event.status == EventStatus::Posted {
            fill_all(tx, &event).await?;
            sync_access(tx, event.id).await?;
        }
        Ok(())
    })
    .await
}

/// Checks reminders, dropping empty ones.
pub(super) fn clean_reminders(input: &[ReminderInput]) -> Result<Vec<ReminderInput>, OpError> {
    let mut reminders = Vec::new();
    for rem in input {
        if rem.offset_minutes == 0 && rem.message.trim().is_empty() {
            continue;
        }
        if !(1..=10_080).contains(&rem.offset_minutes) {
            return Err(OpError::from(crate::msg!("err-reminder-range")));
        }
        if rem.message.chars().count() > 300 {
            return Err(OpError::from(crate::msg!("err-reminder-note")));
        }
        reminders.push(ReminderInput {
            offset_minutes: rem.offset_minutes,
            message: rem.message.trim().to_string(),
        });
    }
    if reminders.len() > 5 {
        return Err(OpError::from(crate::msg!("err-reminders-many")));
    }
    Ok(reminders)
}

/// Checks what a routine's events carry, with the same rules as an event's.
pub(super) async fn clean_extras(
    conn: &mut Connection,
    guild_id: i64,
    extras: &EventExtras,
) -> Result<EventExtras, OpError> {
    let location = extras.location.trim().to_string();
    if location.chars().count() > 200 {
        return Err(OpError::from(crate::msg!("err-location-length")));
    }
    if !valid_color(&extras.color) {
        return Err(OpError::from(crate::msg!("err-color")));
    }
    for role_id in extras
        .mention_role_ids
        .iter()
        .chain(extras.restrict_role_ids.iter())
        .chain(extras.attendee_role_id.iter())
    {
        if must_role(conn, *role_id).await?.guild_id != guild_id {
            return Err(OpError::from(crate::msg!("err-selected-role-elsewhere")));
        }
    }
    if extras.cutoff_offset_minutes.is_some_and(|mins| mins < 0) {
        return Err(OpError::from(crate::msg!("err-signups-close-late")));
    }
    let options = clean_types(conn, guild_id, &extras.options)
        .await?
        .into_iter()
        .map(|opt| OptionInput { id: None, ..opt })
        .collect();
    Ok(EventExtras {
        location,
        options,
        reminders: clean_reminders(&extras.reminders)?,
        ..extras.clone()
    })
}

fn valid_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Insertable)]
#[diesel(table_name = schema::events)]
#[diesel(treat_none_as_default_value = false)]
struct NewEvent<'a> {
    public_id: uuid::Uuid,
    guild_id: i64,
    channel_id: i64,
    title: String,
    description: String,
    location: String,
    starts_at: DateTime<Utc>,
    duration_minutes: i64,
    status: EventStatus,
    color: &'a str,
    image_url: Option<&'a str>,
    cutoff_at: Option<DateTime<Utc>>,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    attendee_role_by: Option<i64>,
    thread_enabled: bool,
    multiple_signups: bool,
    created_by: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn insert_event_row(
    conn: &mut Connection,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<i64, OpError> {
    let title = clean.title.clone();
    let description = clean.description.clone();
    let row = NewEvent {
        public_id: crate::ids::event_id(),
        guild_id: input.guild_id,
        channel_id: input.channel_id,
        title,
        description,
        location: clean.location.clone(),
        starts_at: input.starts_at,
        duration_minutes: input.duration_minutes,
        status: EventStatus::Draft,
        color: &input.color,
        image_url: clean.image_url.as_deref(),
        cutoff_at: input.cutoff_at,
        mention_everyone: input.mention_everyone,
        attendee_role_id: input.attendee_role_id,
        attendee_role_by: clean.attendee_role_by,
        thread_enabled: input.thread_enabled,
        multiple_signups: input.multiple_signups,
        created_by: Some(input.actor_id),
        created_at: now,
        updated_at: now,
    };
    let id = diesel::insert_into(schema::events::table)
        .values(&row)
        .returning(schema::events::id)
        .get_result(conn)
        .await?;
    Ok(id)
}

#[derive(AsChangeset)]
#[diesel(table_name = schema::events)]
#[diesel(treat_none_as_null = true)]
struct EventEdits<'a> {
    channel_id: i64,
    title: String,
    description: String,
    location: String,
    starts_at: DateTime<Utc>,
    duration_minutes: i64,
    color: &'a str,
    image_url: Option<&'a str>,
    cutoff_at: Option<DateTime<Utc>>,
    /// Follows the cutoff: a later one, or none, opens signups again.
    signups_closed: bool,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    attendee_role_by: Option<i64>,
    thread_enabled: bool,
    multiple_signups: bool,
    updated_at: DateTime<Utc>,
}

async fn update_event_row(
    conn: &mut Connection,
    id: i64,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let title = clean.title.clone();
    let description = clean.description.clone();
    let edits = EventEdits {
        channel_id: input.channel_id,
        title,
        description,
        location: clean.location.clone(),
        starts_at: input.starts_at,
        duration_minutes: input.duration_minutes,
        color: &input.color,
        image_url: clean.image_url.as_deref(),
        cutoff_at: input.cutoff_at,
        signups_closed: input.cutoff_at.is_some_and(|cutoff| now >= cutoff),
        mention_everyone: input.mention_everyone,
        attendee_role_id: input.attendee_role_id,
        attendee_role_by: clean.attendee_role_by,
        thread_enabled: input.thread_enabled,
        multiple_signups: input.multiple_signups,
        updated_at: now,
    };
    diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
        .set(&edits)
        .execute(conn)
        .await?;
    Ok(())
}

pub(super) async fn replace_children(
    conn: &mut Connection,
    event_id: i64,
    options: &[OptionInput],
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    sync_options(conn, event_id, options).await?;
    diesel::delete(schema::reminders::table.filter(schema::reminders::event_id.eq(event_id)))
        .execute(conn)
        .await?;
    insert_reminders(conn, event_id, reminders, &[]).await?;
    Ok(())
}

/// Makes an event's options these, in this order. An option with an id stays,
/// with its responses, and takes the new type, capacity, role, and place; one
/// without is added; options left out go, and their responses with them.
pub(super) async fn sync_options(
    conn: &mut Connection,
    event_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    let kept: Vec<i64> = options.iter().filter_map(|opt| opt.id).collect();
    diesel::delete(
        schema::signup_options::table
            .filter(schema::signup_options::event_id.eq(event_id))
            .filter(schema::signup_options::id.ne_all(&kept)),
    )
    .execute(conn)
    .await?;
    for (index, opt) in options.iter().enumerate() {
        let position = index as i64;
        match opt.id {
            Some(id) => {
                diesel::update(
                    schema::signup_options::table
                        .filter(schema::signup_options::id.eq(id))
                        .filter(schema::signup_options::event_id.eq(event_id)),
                )
                .set((
                    schema::signup_options::type_id.eq(opt.type_id),
                    schema::signup_options::capacity.eq(opt.capacity),
                    schema::signup_options::max_guests.eq(opt.max_guests),
                    schema::signup_options::role_id.eq(opt.role_id),
                    schema::signup_options::position.eq(position),
                ))
                .execute(conn)
                .await?;
            }
            None => {
                diesel::insert_into(schema::signup_options::table)
                    .values((
                        schema::signup_options::event_id.eq(event_id),
                        schema::signup_options::type_id.eq(opt.type_id),
                        schema::signup_options::capacity.eq(opt.capacity),
                        schema::signup_options::max_guests.eq(opt.max_guests),
                        schema::signup_options::role_id.eq(opt.role_id),
                        schema::signup_options::position.eq(position),
                    ))
                    .execute(conn)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn replace_reminders(
    conn: &mut Connection,
    event: &store::Event,
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    let old = store::reminders_for(conn, event).await?;
    diesel::delete(schema::reminders::table.filter(schema::reminders::event_id.eq(event.id)))
        .execute(conn)
        .await?;
    insert_reminders(conn, event.id, reminders, &old).await?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::reminders)]
#[diesel(treat_none_as_default_value = false)]
struct NewReminder<'a> {
    event_id: i64,
    offset_minutes: i64,
    message: &'a str,
    sent_at: Option<DateTime<Utc>>,
}

async fn insert_reminders(
    conn: &mut Connection,
    event_id: i64,
    reminders: &[ReminderInput],
    old: &[store::Reminder],
) -> Result<(), OpError> {
    let rows: Vec<_> = reminders
        .iter()
        .map(|rem| {
            let sent_at = old
                .iter()
                .find(|item| {
                    item.offset_minutes == rem.offset_minutes && item.message == rem.message
                })
                .and_then(|item| item.sent_at);
            NewReminder {
                event_id,
                offset_minutes: rem.offset_minutes,
                message: &rem.message,
                sent_at,
            }
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::reminders::table)
        .values(&rows)
        .execute(conn)
        .await?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::event_roles)]
struct NewEventRole {
    event_id: i64,
    role_id: i64,
    kind: EventRoleKind,
}

pub(super) async fn insert_event_roles(
    conn: &mut Connection,
    event_id: i64,
    mention_role_ids: &[i64],
    restrict_role_ids: &[i64],
) -> Result<(), OpError> {
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for role_id in mention_role_ids {
        if seen.insert((*role_id, EventRoleKind::Mention)) {
            rows.push(NewEventRole {
                event_id,
                role_id: *role_id,
                kind: EventRoleKind::Mention,
            });
        }
    }
    for role_id in restrict_role_ids {
        if seen.insert((*role_id, EventRoleKind::Restrict)) {
            rows.push(NewEventRole {
                event_id,
                role_id: *role_id,
                kind: EventRoleKind::Restrict,
            });
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::event_roles::table)
        .values(&rows)
        .execute(conn)
        .await?;
    Ok(())
}

async fn replace_roles(
    conn: &mut Connection,
    event_id: i64,
    input: &EventInput,
) -> Result<(), OpError> {
    diesel::delete(schema::event_roles::table.filter(schema::event_roles::event_id.eq(event_id)))
        .execute(conn)
        .await?;
    insert_event_roles(
        conn,
        event_id,
        &input.mention_role_ids,
        &input.restrict_role_ids,
    )
    .await
}

async fn transition(
    conn: &mut Connection,
    id: i64,
    input: &EventInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let event = must_event(conn, id).await?;
    match (event.status, input.action) {
        (EventStatus::Posted, _) => {
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!("Updated {}.", event.title),
            )
            .await?;
        }
        (
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed,
            SaveAction::Publish,
        ) => {
            mark_posted(conn, id, now).await?;
        }
        (
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed,
            SaveAction::Schedule,
        )
        | (EventStatus::Scheduled, SaveAction::Save) => {
            let post_at = input
                .post_at
                .ok_or_else(|| OpError::from(crate::msg!("err-post-time")))?;
            diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
                .set((
                    schema::events::status.eq(EventStatus::Scheduled),
                    schema::events::post_at.eq(post_at),
                    schema::events::updated_at.eq(now),
                ))
                .execute(conn)
                .await?;
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!("Will post {} at {}.", event.title, clock::stamp(post_at)),
            )
            .await?;
        }
        (
            EventStatus::Draft | EventStatus::Scheduled | EventStatus::Failed,
            SaveAction::Draft | SaveAction::Save,
        ) => {
            diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
                .set((
                    schema::events::status.eq(EventStatus::Draft),
                    schema::events::post_at.eq(None::<DateTime<Utc>>),
                    schema::events::updated_at.eq(now),
                ))
                .execute(conn)
                .await?;
        }
        _ => {
            return Err(OpError::from(crate::msg!("err-event-change")));
        }
    }
    Ok(())
}
