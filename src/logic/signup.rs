use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use super::access::can_edit;
use super::db::{log, must_event, must_option, must_role, must_user, role_names};
use crate::clock;
use crate::error::OpError;
use crate::store::{self, Event};

pub fn signup(
    conn: &mut Connection,
    event_id: i64,
    option_id: i64,
    user_id: i64,
) -> Result<String, OpError> {
    store::with_tx(conn, |tx| signup_tx(tx, event_id, option_id, user_id))
}

fn signup_tx(
    conn: &Connection,
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
        conn.execute("DELETE FROM signups WHERE id = ?", [existing.id])?;
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
            conn.execute("DELETE FROM signups WHERE id = ?", [other.id])?;
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
    conn.execute(
        "INSERT INTO signups (event_id, option_id, user_id, state, created_at) VALUES (?, ?, ?, ?, ?)",
        params![event.id, option.id, user.id, state, clock::stamp(now)],
    )?;
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
                tx.execute("DELETE FROM signups WHERE id = ?", [existing.id])?;
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
                            tx.execute("DELETE FROM signups WHERE id = ?", [other.id])?;
                        }
                    }
                }
                if find_signup(tx, option.id, target.id)?.is_some() {
                    fill_all(tx, event.id)?;
                    sync_access(tx, event.id)?;
                    return Ok(format!("{} is already on {}.", target.name, option.name));
                }
                tx.execute(
                    "INSERT INTO signups (event_id, option_id, user_id, state, created_at) VALUES (?, ?, ?, 'confirmed', ?)",
                    params![event.id, option.id, target.id, store::now_stamp()],
                )?;
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
    conn: &Connection,
    option_id: i64,
    user_id: i64,
) -> Result<Option<store::Signup>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM signups WHERE option_id = ? AND user_id = ?",
            params![option_id, user_id],
            |row| {
                Ok(store::Signup {
                    id: row.get("id")?,
                    event_id: row.get("event_id")?,
                    option_id: row.get("option_id")?,
                    user_id: row.get("user_id")?,
                    state: row.get("state")?,
                    created_at: row.get("created_at")?,
                })
            },
        )
        .ok())
}

fn signups_for_user(
    conn: &Connection,
    event_id: i64,
    user_id: i64,
) -> Result<Vec<store::Signup>, OpError> {
    let mut stmt = conn.prepare("SELECT * FROM signups WHERE event_id = ? AND user_id = ?")?;
    let rows = stmt.query_map(params![event_id, user_id], |row| {
        Ok(store::Signup {
            id: row.get("id")?,
            event_id: row.get("event_id")?,
            option_id: row.get("option_id")?,
            user_id: row.get("user_id")?,
            state: row.get("state")?,
            created_at: row.get("created_at")?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn count_state(conn: &Connection, option_id: i64, state: &str) -> Result<i64, OpError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM signups WHERE option_id = ? AND state = ?",
        params![option_id, state],
        |row| row.get(0),
    )?)
}

pub(super) fn fill_all(conn: &Connection, event_id: i64) -> Result<(), OpError> {
    for opt in store::options_for(conn, event_id)? {
        fill_option(conn, opt.id)?;
    }
    Ok(())
}

fn fill_option(conn: &Connection, option_id: i64) -> Result<(), OpError> {
    let option = must_option(conn, option_id)?;
    let Some(cap) = option.capacity else {
        return Ok(());
    };
    for _ in 0..50 {
        let confirmed = count_state(conn, option_id, "confirmed")?;
        if confirmed >= cap {
            break;
        }
        let next = conn
            .query_row(
                "SELECT id, user_id FROM signups WHERE option_id = ? AND state = 'waitlisted' ORDER BY created_at, id LIMIT 1",
                [option_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .ok();
        let Some((signup_id, user_id)) = next else {
            break;
        };
        conn.execute(
            "UPDATE signups SET state = 'confirmed' WHERE id = ?",
            [signup_id],
        )?;
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

pub(super) fn sync_access(conn: &Connection, event_id: i64) -> Result<(), OpError> {
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
        conn.execute(
            "INSERT OR IGNORE INTO threads (event_id, name) VALUES (?, ?)",
            params![event_id, event.title],
        )?;
        let members = store::thread_member_ids(conn, event_id)?;
        for user_id in &reminding {
            if !members.contains(user_id) {
                conn.execute(
                    "INSERT OR IGNORE INTO thread_members (event_id, user_id) VALUES (?, ?)",
                    params![event_id, user_id],
                )?;
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
                conn.execute(
                    "DELETE FROM thread_members WHERE event_id = ? AND user_id = ?",
                    params![event_id, user_id],
                )?;
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
        conn.execute(
            "INSERT INTO roles (guild_id, name, color) VALUES (?, ?, '#1e3a6e')",
            params![event.guild_id, name],
        )?;
        role_id = Some(conn.last_insert_rowid());
        conn.execute(
            "UPDATE events SET attendee_role_id = ? WHERE id = ?",
            params![role_id, event_id],
        )?;
    }
    if let Some(role_id) = role_id {
        let role_name = must_role(conn, role_id)?.name;
        let grants = store::grant_user_ids(conn, event_id)?;
        for user_id in &reminding {
            if !grants.contains(user_id) {
                conn.execute(
                    "INSERT OR IGNORE INTO role_grants (event_id, user_id, role_id) VALUES (?, ?, ?)",
                    params![event_id, user_id, role_id],
                )?;
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
                conn.execute(
                    "DELETE FROM role_grants WHERE event_id = ? AND user_id = ?",
                    params![event_id, user_id],
                )?;
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
