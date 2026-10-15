use std::collections::HashSet;

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::access::can_edit;
use super::db::{log, must_event, must_option, must_role, must_user, role_names};
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event};

pub fn signup(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    user_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| signup_tx(tx, event_id, option_id, user_id))
}

fn signup_tx(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    user_id: i64,
) -> Result<String, OpError> {
    let event = must_event(conn, event_id)?;
    let user = must_user(conn, user_id)?;
    if user.guild_id != event.guild_id {
        return Err(OpError::message("That person isn't in this server."));
    }
    let option = must_option(conn, option_id)?;
    if option.event_id != event.id {
        return Err(OpError::NotFound);
    }
    if event.status != "posted" {
        return Err(OpError::message("Signups open once the event is posted."));
    }
    let now = Utc::now();
    if let Some(existing) = find_signup(conn, option_id, user_id)? {
        let state = existing.state.clone();
        diesel::delete(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
            .execute(conn)?;
        fill_all(conn, event.id)?;
        sync_access(conn, event.id)?;
        let notice = if state == "waitlisted" {
            format!("{} left the {} waitlist.", user.name, option.name)
        } else {
            format!("{} left {}.", user.name, option.name)
        };
        log(conn, event.guild_id, Some(event.id), &notice)?;
        return Ok(notice);
    }
    if event.signups_closed || cutoff_passed(&event, now) {
        return Err(OpError::message(format!(
            "Signups for {} are closed.",
            event.title
        )));
    }
    let restricts = store::event_role_ids(conn, event.id, "restrict")?;
    if !restricts.is_empty() && !user.organizer {
        let roles = store::user_role_ids(conn, user.id)?;
        if !restricts.iter().any(|role| roles.contains(role)) {
            let names = role_names(conn, &restricts)?;
            return Err(OpError::message(format!(
                "This event is limited to {}.",
                names.join(", ")
            )));
        }
    }
    if let Some(role_id) = option.role_id {
        if !user.organizer && !store::user_has_role(conn, user.id, role_id)? {
            let role = must_role(conn, role_id)?;
            return Err(OpError::message(format!(
                "{} can't take a {} spot. That option is limited to {}.",
                user.name, option.name, role.name
            )));
        }
    }
    let moved = if !event.multiple_signups {
        let others = signups_for_user(conn, event.id, user.id)?;
        let moved = !others.is_empty();
        for other in others {
            diesel::delete(schema::signups::table.filter(schema::signups::id.eq(other.id)))
                .execute(conn)?;
        }
        moved
    } else {
        false
    };
    let option = must_option(conn, option_id)?;
    let confirmed = count_state(conn, option.id, "confirmed")?;
    let state = if let Some(cap) = option.capacity {
        if confirmed < cap {
            "confirmed"
        } else if option.waitlist {
            "waitlisted"
        } else {
            return Err(OpError::message(format!("{} is full.", option.name)));
        }
    } else {
        "confirmed"
    };
    diesel::insert_into(schema::signups::table)
        .values((
            schema::signups::event_id.eq(event.id),
            schema::signups::option_id.eq(option.id),
            schema::signups::user_id.eq(user.id),
            schema::signups::state.eq(state),
            schema::signups::created_at.eq(clock::stamp(now)),
        ))
        .execute(conn)?;
    fill_all(conn, event.id)?;
    sync_access(conn, event.id)?;
    let notice = if moved && state == "confirmed" {
        format!("{} moved to {}.", user.name, option.name)
    } else if state == "waitlisted" {
        format!("{} is waiting for a {} spot.", user.name, option.name)
    } else {
        format!("{} signed up as {}.", user.name, option.name)
    };
    log(conn, event.guild_id, Some(event.id), &notice)?;
    Ok(notice)
}

pub fn organizer_response(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
    target_id: i64,
    option_id: i64,
    action: &str,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| {
        let event = must_event(tx, event_id)?;
        let actor = must_user(tx, actor_id)?;
        let settings = store::settings(tx, event.guild_id)?;
        let roles = store::user_role_ids(tx, actor.id)?;
        if !can_edit(&actor, &event, &settings, &roles) {
            return Err(OpError::message("You can't change these responses."));
        }
        if event.status != "posted" {
            return Err(OpError::message(
                "Post the event before editing its responses.",
            ));
        }
        let target = must_user(tx, target_id)?;
        let option = must_option(tx, option_id)?;
        if option.event_id != event.id || target.guild_id != event.guild_id {
            return Err(OpError::NotFound);
        }
        match action {
            "remove" => {
                let Some(existing) = find_signup(tx, option.id, target.id)? else {
                    return Ok(format!("{} wasn't on {}.", target.name, option.name));
                };
                diesel::delete(schema::signups::table.filter(schema::signups::id.eq(existing.id)))
                    .execute(tx)?;
                fill_all(tx, event.id)?;
                sync_access(tx, event.id)?;
                let notice = format!("Removed {} from {}.", target.name, option.name);
                log(tx, event.guild_id, Some(event.id), &notice)?;
                Ok(notice)
            }
            "add" | "move" => {
                if !event.multiple_signups || action == "move" {
                    for other in signups_for_user(tx, event.id, target.id)? {
                        if other.option_id != option.id {
                            diesel::delete(
                                schema::signups::table.filter(schema::signups::id.eq(other.id)),
                            )
                            .execute(tx)?;
                        }
                    }
                }
                if find_signup(tx, option.id, target.id)?.is_some() {
                    fill_all(tx, event.id)?;
                    sync_access(tx, event.id)?;
                    return Ok(format!("{} is already on {}.", target.name, option.name));
                }
                diesel::insert_into(schema::signups::table)
                    .values((
                        schema::signups::event_id.eq(event.id),
                        schema::signups::option_id.eq(option.id),
                        schema::signups::user_id.eq(target_id),
                        schema::signups::state.eq("confirmed"),
                        schema::signups::created_at.eq(store::now_stamp()),
                    ))
                    .execute(tx)?;
                fill_all(tx, event.id)?;
                sync_access(tx, event.id)?;
                let notice = format!("Added {} to {}.", target.name, option.name);
                log(tx, event.guild_id, Some(event.id), &notice)?;
                Ok(notice)
            }
            _ => Err(OpError::message("Use add, remove, or move.")),
        }
    })
}

fn find_signup(
    conn: &mut Connection,
    option_id: i64,
    user_id: i64,
) -> Result<Option<store::Signup>, OpError> {
    Ok(schema::signups::table
        .filter(schema::signups::option_id.eq(option_id))
        .filter(schema::signups::user_id.eq(user_id))
        .select(store::Signup::as_select())
        .first(conn)
        .optional()?)
}

fn signups_for_user(
    conn: &mut Connection,
    event_id: i64,
    user_id: i64,
) -> Result<Vec<store::Signup>, OpError> {
    Ok(schema::signups::table
        .filter(schema::signups::event_id.eq(event_id))
        .filter(schema::signups::user_id.eq(user_id))
        .select(store::Signup::as_select())
        .load(conn)?)
}

fn count_state(conn: &mut Connection, option_id: i64, state: &str) -> Result<i64, OpError> {
    Ok(schema::signups::table
        .filter(schema::signups::option_id.eq(option_id))
        .filter(schema::signups::state.eq(state))
        .count()
        .get_result(conn)?)
}

pub(super) fn fill_all(conn: &mut Connection, event_id: i64) -> Result<(), OpError> {
    for opt in store::options_for(conn, event_id)? {
        fill_option(conn, opt.id)?;
    }
    Ok(())
}

fn fill_option(conn: &mut Connection, option_id: i64) -> Result<(), OpError> {
    let option = must_option(conn, option_id)?;
    let Some(cap) = option.capacity else {
        return Ok(());
    };
    for _ in 0..50 {
        let confirmed = count_state(conn, option_id, "confirmed")?;
        if confirmed >= cap {
            break;
        }
        let next: Option<(i64, i64)> = schema::signups::table
            .filter(schema::signups::option_id.eq(option_id))
            .filter(schema::signups::state.eq("waitlisted"))
            .order((schema::signups::created_at.asc(), schema::signups::id.asc()))
            .select((schema::signups::id, schema::signups::user_id))
            .first(conn)
            .optional()?;
        let Some((signup_id, user_id)) = next else {
            break;
        };
        diesel::update(schema::signups::table.filter(schema::signups::id.eq(signup_id)))
            .set(schema::signups::state.eq("confirmed"))
            .execute(conn)?;
        let user = must_user(conn, user_id)?;
        let event = must_event(conn, option.event_id)?;
        log(
            conn,
            event.guild_id,
            Some(event.id),
            &format!(
                "Marian pulled {} off the {} waitlist.",
                user.name, option.name
            ),
        )?;
    }
    Ok(())
}

pub(super) fn sync_access(conn: &mut Connection, event_id: i64) -> Result<(), OpError> {
    let event = must_event(conn, event_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    let options = store::options_for(conn, event_id)?;
    let signups = store::signups_for(conn, event_id)?;
    let mut reminding = HashSet::new();
    for signup in &signups {
        if signup.state != "confirmed" {
            continue;
        }
        if options
            .iter()
            .any(|opt| opt.id == signup.option_id && opt.reminders_enabled)
        {
            reminding.insert(signup.user_id);
        }
    }
    if event.thread_enabled && settings.auto_join_threads {
        diesel::insert_into(schema::threads::table)
            .values((
                schema::threads::event_id.eq(event_id),
                schema::threads::name.eq(&super::discord::stored_thread_name(conn, &event)?),
            ))
            .on_conflict_do_nothing()
            .execute(conn)?;
        let members = store::thread_member_ids(conn, event_id)?;
        for user_id in &reminding {
            if !members.contains(user_id) {
                diesel::insert_into(schema::thread_members::table)
                    .values((
                        schema::thread_members::event_id.eq(event_id),
                        schema::thread_members::user_id.eq(user_id),
                    ))
                    .on_conflict_do_nothing()
                    .execute(conn)?;
                let user = must_user(conn, *user_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!("Marian added {} to the thread.", user.name),
                )?;
            }
        }
        for user_id in members {
            if !reminding.contains(&user_id) {
                diesel::delete(
                    schema::thread_members::table
                        .filter(schema::thread_members::event_id.eq(event_id))
                        .filter(schema::thread_members::user_id.eq(user_id)),
                )
                .execute(conn)?;
                let user = must_user(conn, user_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!("Marian removed {} from the thread.", user.name),
                )?;
            }
        }
    }

    let mut role_id = event.attendee_role_id;
    if role_id.is_none() && settings.temp_roles && !reminding.is_empty() {
        let name: String = format!("{} attendee", event.title)
            .chars()
            .take(80)
            .collect();
        let new_id = diesel::insert_into(schema::roles::table)
            .values((
                schema::roles::guild_id.eq(event.guild_id),
                schema::roles::name.eq(name),
                schema::roles::color.eq("#1e3a6e"),
            ))
            .returning(schema::roles::id)
            .get_result(conn)?;
        role_id = Some(new_id);
        diesel::update(schema::events::table.filter(schema::events::id.eq(event_id)))
            .set(schema::events::attendee_role_id.eq(role_id))
            .execute(conn)?;
    }
    if let Some(role_id) = role_id {
        let role_name = must_role(conn, role_id)?.name;
        let grants = store::grant_user_ids(conn, event_id)?;
        for user_id in &reminding {
            if !grants.contains(user_id) {
                diesel::insert_into(schema::role_grants::table)
                    .values((
                        schema::role_grants::event_id.eq(event_id),
                        schema::role_grants::user_id.eq(user_id),
                        schema::role_grants::role_id.eq(role_id),
                    ))
                    .on_conflict_do_nothing()
                    .execute(conn)?;
                let user = must_user(conn, *user_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!("Marian gave {} the {} role.", user.name, role_name),
                )?;
            }
        }
        for user_id in grants {
            if !reminding.contains(&user_id) {
                diesel::delete(
                    schema::role_grants::table
                        .filter(schema::role_grants::event_id.eq(event_id))
                        .filter(schema::role_grants::user_id.eq(user_id)),
                )
                .execute(conn)?;
                let user = must_user(conn, user_id)?;
                log(
                    conn,
                    event.guild_id,
                    Some(event_id),
                    &format!(
                        "Marian took the {} role back from {}.",
                        role_name, user.name
                    ),
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn cutoff_passed(event: &Event, now: DateTime<Utc>) -> bool {
    event
        .cutoff_at
        .as_deref()
        .and_then(|value| clock::parse_stamp(value).ok())
        .map(|cutoff| now >= cutoff)
        .unwrap_or(false)
}
