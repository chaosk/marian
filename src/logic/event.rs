use std::collections::HashSet;

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::access::{can_create, can_edit};
use super::db::{log, must_channel, must_event, must_role, must_user};
use super::model::*;
use super::series::{apply_series_template, ensure_series, link_new_series, mark_posted};
use super::signup::{fill_all, sync_access};
use super::template;
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};

pub fn save_event(conn: &mut Connection, input: EventInput) -> Result<i64, OpError> {
    let clean = normalize(conn, &input)?;
    let now = Utc::now();
    store::with_tx(conn, |tx| {
        let id = if let Some(existing) = &clean.existing {
            update_event_row(tx, existing.id, &input, &clean, now)?;
            if existing.status == "posted" {
                update_options_in_place(tx, existing.id, &clean.options)?;
                replace_reminders(tx, existing.id, &clean.reminders)?;
            } else {
                replace_children(tx, existing.id, &clean.options, &clean.reminders)?;
            }
            replace_roles(tx, existing.id, &input)?;
            existing.id
        } else {
            let id = insert_event_row(tx, &input, &clean, now)?;
            replace_children(tx, id, &clean.options, &clean.reminders)?;
            replace_roles(tx, id, &input)?;
            id
        };

        transition(tx, id, &input, now)?;

        let event = must_event(tx, id)?;
        if input.repeat.is_some()
            && event.series_id.is_none()
            && clean.existing.as_ref().and_then(|e| e.series_id).is_none()
        {
            link_new_series(tx, &event, &input, &clean, now)?;
        }
        let event = must_event(tx, id)?;
        if input.apply_series {
            if let Some(series_id) = event.series_id {
                apply_series_template(tx, series_id, id, &input, &clean)?;
            }
        }
        let event = must_event(tx, id)?;
        if let Some(series_id) = event.series_id {
            if matches!(event.status.as_str(), "posted" | "scheduled" | "archived") {
                ensure_series(tx, series_id, now)?;
            }
        }
        if event.status == "posted" {
            fill_all(tx, id)?;
            sync_access(tx, id)?;
        }
        Ok(id)
    })
}

fn normalize(conn: &mut Connection, input: &EventInput) -> Result<CleanInput, OpError> {
    let settings = store::settings(conn, input.guild_id)?;
    let actor = must_user(conn, input.actor_id)?;
    if actor.guild_id != input.guild_id {
        return Err(OpError::message("That person isn't in this server."));
    }
    let roles = store::user_role_ids(conn, actor.id)?;
    let existing = if let Some(id) = input.id {
        let event = must_event(conn, id)?;
        if event.guild_id != input.guild_id {
            return Err(OpError::NotFound);
        }
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message(
                "You can edit events you created, or any event if you run the server.",
            ));
        }
        if matches!(event.status.as_str(), "archived" | "skipped" | "cancelled") {
            return Err(OpError::message(
                "That date is already closed. Duplicate it if you need another one.",
            ));
        }
        Some(event)
    } else {
        if !can_create(&actor, &settings, &roles) {
            return Err(OpError::message(
                "You don't have the role required to create events.",
            ));
        }
        None
    };

    let title = input.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(OpError::message(
            "Give the event a title, up to 120 characters.",
        ));
    }
    let description = input.description.trim().to_string();
    if description.chars().count() > 2000 {
        return Err(OpError::message(
            "Keep the description under 2000 characters.",
        ));
    }
    if !(15..=1440).contains(&input.duration_minutes) {
        return Err(OpError::message(
            "Duration needs to be between 15 minutes and 24 hours.",
        ));
    }
    if !valid_color(&input.color) {
        return Err(OpError::message("Pick one of the event colors."));
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
        Some(_) => {
            return Err(OpError::message(
                "Image links need to start with http:// or https://.",
            ))
        }
    };
    let channel = must_channel(conn, input.channel_id)?;
    if channel.guild_id != input.guild_id {
        return Err(OpError::message("Pick a channel from this server."));
    }
    if let Some(role_id) = input.attendee_role_id {
        let role = must_role(conn, role_id)?;
        if role.guild_id != input.guild_id {
            return Err(OpError::message(
                "The attendee role isn't from this server.",
            ));
        }
    }
    for role_id in input
        .mention_role_ids
        .iter()
        .chain(input.restrict_role_ids.iter())
    {
        let role = must_role(conn, *role_id)?;
        if role.guild_id != input.guild_id {
            return Err(OpError::message("A selected role isn't from this server."));
        }
    }
    if let Some(cutoff) = input.cutoff_at {
        if cutoff > input.starts_at {
            return Err(OpError::message(
                "The signup cutoff has to be before the event starts.",
            ));
        }
    }

    let mut options = Vec::new();
    for opt in &input.options {
        let name = opt.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if name.chars().count() > 40 {
            return Err(OpError::message(
                "Signup option names stay under 40 characters.",
            ));
        }
        let emoji = {
            let raw = opt.emoji.trim();
            let raw = if raw.is_empty() { "•" } else { raw };
            if raw.chars().count() > 8 {
                return Err(OpError::message("Keep each option emoji short."));
            }
            raw.to_string()
        };
        if let Some(cap) = opt.capacity {
            if cap < 1 || cap > 500 {
                return Err(OpError::message(
                    "Capacity has to be at least 1, or left open.",
                ));
            }
        }
        if opt.waitlist && opt.capacity.is_none() {
            return Err(OpError::message(format!(
                "{name} has a waitlist but no capacity. Set a cap, or turn the waitlist off."
            )));
        }
        if let Some(role_id) = opt.role_id {
            let role = must_role(conn, role_id)?;
            if role.guild_id != input.guild_id {
                return Err(OpError::message(
                    "An option is limited to a role from another server.",
                ));
            }
        }
        options.push(OptionInput {
            id: opt.id,
            name,
            emoji,
            capacity: opt.capacity,
            waitlist: opt.waitlist,
            role_id: opt.role_id,
            reminders_enabled: opt.reminders_enabled,
        });
    }
    if options.is_empty() {
        return Err(OpError::message("Add at least one signup option."));
    }
    if options.len() > 8 {
        return Err(OpError::message(
            "An event can have up to 8 signup options.",
        ));
    }
    if let Some(existing) = &existing {
        if existing.status == "posted" {
            let current = store::options_for(conn, existing.id)?;
            let ids: HashSet<i64> = current.iter().map(|opt| opt.id).collect();
            let same = options.len() == current.len()
                && options
                    .iter()
                    .all(|opt| opt.id.map(|id| ids.contains(&id)).unwrap_or(false));
            if !same {
                return Err(OpError::message(
                    "Signup options can't be added or removed after the event is posted. You can still rename them and change capacity.",
                ));
            }
        }
    }

    let mut reminders = Vec::new();
    for rem in &input.reminders {
        if rem.offset_minutes == 0 && rem.message.trim().is_empty() {
            continue;
        }
        if !(1..=10_080).contains(&rem.offset_minutes) {
            return Err(OpError::message(
                "Reminders need to be between 1 minute and 7 days out.",
            ));
        }
        if rem.message.chars().count() > 300 {
            return Err(OpError::message(
                "Keep reminder notes under 300 characters.",
            ));
        }
        reminders.push(ReminderInput {
            offset_minutes: rem.offset_minutes,
            message: rem.message.trim().to_string(),
        });
    }
    if reminders.is_empty() && !matches!(input.action, SaveAction::Draft) {
        reminders.push(ReminderInput {
            offset_minutes: settings.default_reminder_minutes.clamp(1, 10_080),
            message: String::new(),
        });
    }
    if reminders.len() > 5 {
        return Err(OpError::message("An event can have up to 5 reminders."));
    }

    if let Some(repeat) = &input.repeat {
        if !matches!(
            repeat.cadence.as_str(),
            "daily" | "weekly" | "biweekly" | "monthly"
        ) {
            return Err(OpError::message(
                "Repeat daily, weekly, every two weeks, or monthly.",
            ));
        }
        if let Some(max) = repeat.max_occurrences {
            if max < 2 {
                return Err(OpError::message(
                    "A series needs at least 2 dates. Leave the count empty to keep going.",
                ));
            }
        }
        if let Some(end) = repeat.end_on {
            if end < input.starts_at {
                return Err(OpError::message(
                    "The series end date is before the first event.",
                ));
            }
        }
    }
    if matches!(input.action, SaveAction::Schedule) {
        let post_at = input
            .post_at
            .ok_or_else(|| OpError::message("Pick a time for Marian to post this."))?;
        if post_at <= Utc::now() {
            return Err(OpError::message(
                "That post time is already past. Post it now, or pick a later time.",
            ));
        }
        if post_at >= input.starts_at {
            return Err(OpError::message("Post the event before it starts."));
        }
    }

    Ok(CleanInput {
        title,
        description,
        image_url,
        options,
        reminders,
        existing,
    })
}

fn valid_color(color: &str) -> bool {
    let bytes = color.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}

fn insert_event_row(
    conn: &mut Connection,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<i64, OpError> {
    let stamp = clock::stamp(now);
    let title = template::fill_text(conn, input.guild_id, &clean.title, input.starts_at)?;
    let description =
        template::fill_text(conn, input.guild_id, &clean.description, input.starts_at)?;
    let id = diesel::insert_into(schema::events::table)
        .values((
            schema::events::public_id.eq(crate::ids::event_id()),
            schema::events::guild_id.eq(input.guild_id),
            schema::events::channel_id.eq(input.channel_id),
            schema::events::title.eq(title),
            schema::events::description.eq(description),
            schema::events::starts_at.eq(clock::stamp(input.starts_at)),
            schema::events::duration_minutes.eq(input.duration_minutes),
            schema::events::status.eq("draft"),
            schema::events::color.eq(&input.color),
            schema::events::image_url.eq(clean.image_url.as_deref()),
            schema::events::cutoff_at.eq(input.cutoff_at.map(|dt| clock::stamp(dt))),
            schema::events::mention_everyone.eq(input.mention_everyone),
            schema::events::attendee_role_id.eq(input.attendee_role_id),
            schema::events::thread_enabled.eq(input.thread_enabled),
            schema::events::multiple_signups.eq(input.multiple_signups),
            schema::events::created_by.eq(input.actor_id),
            schema::events::created_at.eq(&stamp),
            schema::events::updated_at.eq(&stamp),
        ))
        .returning(schema::events::id)
        .get_result(conn)?;
    Ok(id)
}

fn update_event_row(
    conn: &mut Connection,
    id: i64,
    input: &EventInput,
    clean: &CleanInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let title = template::fill_text(conn, input.guild_id, &clean.title, input.starts_at)?;
    let description =
        template::fill_text(conn, input.guild_id, &clean.description, input.starts_at)?;
    diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
        .set((
            schema::events::channel_id.eq(input.channel_id),
            schema::events::title.eq(title),
            schema::events::description.eq(description),
            schema::events::starts_at.eq(clock::stamp(input.starts_at)),
            schema::events::duration_minutes.eq(input.duration_minutes),
            schema::events::color.eq(&input.color),
            schema::events::image_url.eq(clean.image_url.as_deref()),
            schema::events::cutoff_at.eq(input.cutoff_at.map(|dt| clock::stamp(dt))),
            schema::events::mention_everyone.eq(input.mention_everyone),
            schema::events::attendee_role_id.eq(input.attendee_role_id),
            schema::events::thread_enabled.eq(input.thread_enabled),
            schema::events::multiple_signups.eq(input.multiple_signups),
            schema::events::updated_at.eq(clock::stamp(now)),
        ))
        .execute(conn)?;
    Ok(())
}

pub(super) fn replace_children(
    conn: &mut Connection,
    event_id: i64,
    options: &[OptionInput],
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    diesel::delete(schema::signups::table.filter(schema::signups::event_id.eq(event_id)))
        .execute(conn)?;
    diesel::delete(
        schema::signup_options::table.filter(schema::signup_options::event_id.eq(event_id)),
    )
    .execute(conn)?;
    diesel::delete(schema::reminders::table.filter(schema::reminders::event_id.eq(event_id)))
        .execute(conn)?;
    insert_options(conn, event_id, options)?;
    insert_reminders(conn, event_id, reminders, &[])?;
    Ok(())
}

fn update_options_in_place(
    conn: &mut Connection,
    event_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    for (index, opt) in options.iter().enumerate() {
        let Some(id) = opt.id else {
            return Err(OpError::message("A posted option is missing its id."));
        };
        diesel::update(
            schema::signup_options::table
                .filter(schema::signup_options::id.eq(id))
                .filter(schema::signup_options::event_id.eq(event_id)),
        )
        .set((
            schema::signup_options::name.eq(&opt.name),
            schema::signup_options::emoji.eq(&opt.emoji),
            schema::signup_options::capacity.eq(opt.capacity),
            schema::signup_options::waitlist.eq(opt.waitlist),
            schema::signup_options::role_id.eq(opt.role_id),
            schema::signup_options::reminders_enabled.eq(opt.reminders_enabled),
            schema::signup_options::position.eq(index as i64),
        ))
        .execute(conn)?;
    }
    Ok(())
}

fn replace_reminders(
    conn: &mut Connection,
    event_id: i64,
    reminders: &[ReminderInput],
) -> Result<(), OpError> {
    let old = store::reminders_for(conn, event_id)?;
    diesel::delete(schema::reminders::table.filter(schema::reminders::event_id.eq(event_id)))
        .execute(conn)?;
    insert_reminders(conn, event_id, reminders, &old)?;
    Ok(())
}

fn insert_options(
    conn: &mut Connection,
    event_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    for (index, opt) in options.iter().enumerate() {
        diesel::insert_into(schema::signup_options::table)
            .values((
                schema::signup_options::event_id.eq(event_id),
                schema::signup_options::name.eq(&opt.name),
                schema::signup_options::emoji.eq(&opt.emoji),
                schema::signup_options::capacity.eq(opt.capacity),
                schema::signup_options::waitlist.eq(opt.waitlist),
                schema::signup_options::role_id.eq(opt.role_id),
                schema::signup_options::reminders_enabled.eq(opt.reminders_enabled),
                schema::signup_options::position.eq(index as i64),
            ))
            .execute(conn)?;
    }
    Ok(())
}

fn insert_reminders(
    conn: &mut Connection,
    event_id: i64,
    reminders: &[ReminderInput],
    old: &[store::Reminder],
) -> Result<(), OpError> {
    for rem in reminders {
        let sent = old
            .iter()
            .find(|item| item.offset_minutes == rem.offset_minutes && item.message == rem.message)
            .and_then(|item| item.sent_at.clone());
        diesel::insert_into(schema::reminders::table)
            .values((
                schema::reminders::event_id.eq(event_id),
                schema::reminders::offset_minutes.eq(rem.offset_minutes),
                schema::reminders::message.eq(&rem.message),
                schema::reminders::sent_at.eq(sent),
            ))
            .execute(conn)?;
    }
    Ok(())
}

fn replace_roles(conn: &mut Connection, event_id: i64, input: &EventInput) -> Result<(), OpError> {
    diesel::delete(schema::event_roles::table.filter(schema::event_roles::event_id.eq(event_id)))
        .execute(conn)?;
    for role_id in &input.mention_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(event_id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("mention"),
            ))
            .execute(conn)?;
    }
    for role_id in &input.restrict_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(event_id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("restrict"),
            ))
            .on_conflict_do_nothing()
            .execute(conn)?;
    }
    Ok(())
}

fn transition(
    conn: &mut Connection,
    id: i64,
    input: &EventInput,
    now: DateTime<Utc>,
) -> Result<(), OpError> {
    let event = must_event(conn, id)?;
    let stamp = clock::stamp(now);
    match (event.status.as_str(), input.action) {
        ("posted", _) => {
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!(
                    "Updated {}. The posted message now shows the new details.",
                    event.title
                ),
            )?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Publish) => {
            mark_posted(conn, id, now)?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Schedule)
        | ("scheduled", SaveAction::Save) => {
            let post_at = input
                .post_at
                .ok_or_else(|| OpError::message("Pick a time for Marian to post this."))?;
            diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
                .set((
                    schema::events::status.eq("scheduled"),
                    schema::events::post_at.eq(clock::stamp(post_at)),
                    schema::events::updated_at.eq(&stamp),
                ))
                .execute(conn)?;
            log(
                conn,
                event.guild_id,
                Some(id),
                &format!("Will post {} at {}.", event.title, clock::stamp(post_at)),
            )?;
        }
        ("draft" | "scheduled" | "failed", SaveAction::Draft | SaveAction::Save) => {
            diesel::update(schema::events::table.filter(schema::events::id.eq(id)))
                .set((
                    schema::events::status.eq("draft"),
                    schema::events::post_at.eq(None::<String>),
                    schema::events::updated_at.eq(&stamp),
                ))
                .execute(conn)?;
        }
        _ => {
            return Err(OpError::message("That event can't take this change."));
        }
    }
    Ok(())
}
