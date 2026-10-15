use diesel::prelude::*;
use diesel::upsert::excluded;

use super::access::{can_create, can_edit};
use super::db::{log, must_event, must_user};
use super::model::*;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};

pub fn set_user_reminders(
    conn: &mut Connection,
    user_id: i64,
    enabled: bool,
) -> Result<String, OpError> {
    let user = must_user(conn, user_id)?;
    diesel::update(schema::users::table.filter(schema::users::id.eq(user_id)))
        .set(schema::users::reminders_enabled.eq(enabled))
        .execute(conn)?;
    Ok(if enabled {
        format!("Reminders are on for {}.", user.name)
    } else {
        format!("{} won't be pinged for reminders.", user.name)
    })
}

pub fn create_channel(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    name: &str,
    kind: &str,
) -> Result<i64, OpError> {
    let actor = must_user(conn, actor_id)?;
    let settings = store::settings(conn, guild_id)?;
    let roles = store::user_role_ids(conn, actor.id)?;
    let allowed = actor.organizer
        || match settings.role_channel {
            Some(role) => roles.contains(&role),
            None => can_create(&actor, &settings, &roles),
        };
    if !allowed {
        return Err(OpError::message("You can't create channels here."));
    }
    let slug = slug_channel(name);
    if slug.is_empty() {
        return Err(OpError::message("Give the channel a name."));
    }
    if store::channel_by_name(conn, guild_id, &slug)?.is_some() {
        return Err(OpError::message(format!("#{slug} already exists.")));
    }
    let kind = if kind == "event" { "event" } else { "text" };
    let position = schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .select(diesel::dsl::max(schema::channels::position))
        .first::<Option<i64>>(conn)?
        .unwrap_or(0)
        + 1;
    let id = diesel::insert_into(schema::channels::table)
        .values((
            schema::channels::guild_id.eq(guild_id),
            schema::channels::name.eq(&slug),
            schema::channels::kind.eq(kind),
            schema::channels::position.eq(position),
        ))
        .returning(schema::channels::id)
        .get_result(conn)?;
    log(conn, guild_id, None, &format!("Created #{slug}."))?;
    Ok(id)
}

pub fn slug_channel(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.trim().trim_start_matches('#').chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

pub fn save_preset(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    name: &str,
    body: &PresetBody,
) -> Result<i64, OpError> {
    let actor = must_user(conn, actor_id)?;
    if !actor.organizer && actor.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    let settings = store::settings(conn, guild_id)?;
    let roles = store::user_role_ids(conn, actor.id)?;
    let allowed = actor.organizer
        || settings
            .role_signup
            .map(|role| roles.contains(&role))
            .unwrap_or(true)
            && can_create(&actor, &settings, &roles);
    if !allowed {
        return Err(OpError::message("You can't edit signup presets."));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(OpError::message("Name the preset."));
    }
    if body.options.is_empty() {
        return Err(OpError::message("A preset needs at least one option."));
    }
    let json = serde_json::to_string(body).map_err(|err| OpError::message(err.to_string()))?;
    let id = diesel::insert_into(schema::presets::table)
        .values((
            schema::presets::public_id.eq(crate::ids::preset_id()),
            schema::presets::guild_id.eq(guild_id),
            schema::presets::name.eq(name),
            schema::presets::body.eq(json),
        ))
        .returning(schema::presets::id)
        .get_result(conn)?;
    Ok(id)
}

pub fn delete_preset(conn: &mut Connection, preset_id: i64, actor_id: i64) -> Result<(), OpError> {
    let preset = store::preset(conn, preset_id)?.ok_or(OpError::NotFound)?;
    let actor = must_user(conn, actor_id)?;
    if actor.guild_id != preset.guild_id {
        return Err(OpError::NotFound);
    }
    if !actor.organizer {
        let settings = store::settings(conn, preset.guild_id)?;
        let roles = store::user_role_ids(conn, actor.id)?;
        let allowed = settings
            .role_signup
            .map(|role| roles.contains(&role))
            .unwrap_or(false);
        if !allowed {
            return Err(OpError::message("You can't remove presets."));
        }
    }
    diesel::delete(schema::presets::table.filter(schema::presets::id.eq(preset_id)))
        .execute(conn)?;
    Ok(())
}

pub fn library_add(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    name: &str,
    emoji: &str,
) -> Result<(), OpError> {
    require_option_editor(conn, guild_id, actor_id)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(OpError::message("Name the option."));
    }
    let emoji = if emoji.trim().is_empty() {
        "•"
    } else {
        emoji.trim()
    };
    diesel::insert_into(schema::option_library::table)
        .values((
            schema::option_library::guild_id.eq(guild_id),
            schema::option_library::name.eq(name),
            schema::option_library::emoji.eq(emoji),
        ))
        .on_conflict((
            schema::option_library::guild_id,
            schema::option_library::name,
        ))
        .do_update()
        .set(schema::option_library::emoji.eq(excluded(schema::option_library::emoji)))
        .execute(conn)?;
    Ok(())
}

pub fn library_remove(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
    name: &str,
) -> Result<(), OpError> {
    require_option_editor(conn, guild_id, actor_id)?;
    diesel::delete(
        schema::option_library::table
            .filter(schema::option_library::guild_id.eq(guild_id))
            .filter(schema::lower(schema::option_library::name).eq(schema::lower(name.trim()))),
    )
    .execute(conn)?;
    Ok(())
}

fn require_option_editor(
    conn: &mut Connection,
    guild_id: i64,
    actor_id: i64,
) -> Result<(), OpError> {
    let actor = must_user(conn, actor_id)?;
    if actor.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    if actor.organizer {
        return Ok(());
    }
    let settings = store::settings(conn, guild_id)?;
    let roles = store::user_role_ids(conn, actor.id)?;
    let allowed = settings
        .role_signup
        .map(|role| roles.contains(&role))
        .unwrap_or_else(|| can_create(&actor, &settings, &roles));
    if allowed {
        Ok(())
    } else {
        Err(OpError::message(
            "You can't edit the signup option library.",
        ))
    }
}

pub fn manual_calendar_sync(
    conn: &mut Connection,
    event_id: i64,
    actor_id: i64,
) -> Result<String, OpError> {
    let event = must_event(conn, event_id)?;
    let actor = must_user(conn, actor_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    let roles = store::user_role_ids(conn, actor.id)?;
    if !can_edit(&actor, &event, &settings, &roles) {
        return Err(OpError::message("You can't sync this event."));
    }
    if !matches!(event.status.as_str(), "posted" | "scheduled") {
        return Err(OpError::message(
            "Post or schedule the event before syncing it.",
        ));
    }
    let stamp = store::now_stamp();
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set(schema::events::calendar_synced_at.eq(stamp))
        .execute(conn)?;
    let notice = format!("Marked {} for the calendar feed.", event.title);
    log(conn, event.guild_id, Some(event.id), &notice)?;
    Ok(notice)
}

pub fn record_command(
    conn: &mut Connection,
    guild_id: i64,
    user_id: i64,
    line: &str,
    reply: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::command_log::table)
        .values((
            schema::command_log::guild_id.eq(guild_id),
            schema::command_log::user_id.eq(user_id),
            schema::command_log::line.eq(line),
            schema::command_log::reply.eq(reply),
            schema::command_log::created_at.eq(store::now_stamp()),
        ))
        .execute(conn)?;
    Ok(())
}
