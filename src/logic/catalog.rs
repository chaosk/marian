use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::upsert::excluded;

use super::access::{can_create, can_edit};
use super::db::{log, must_event, must_membership};
use super::model::*;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};

pub fn set_user_reminders(
    conn: &mut Connection,
    membership_id: i64,
    enabled: bool,
) -> Result<String, OpError> {
    let user = must_membership(conn, membership_id)?;
    diesel::update(schema::memberships::table.filter(schema::memberships::id.eq(membership_id)))
        .set(schema::memberships::reminders_enabled.eq(enabled))
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
) -> Result<i64, OpError> {
    let actor = must_membership(conn, actor_id)?;
    let settings = store::settings(conn, guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
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
    if let Some(existing) = store::channel_by_name(conn, guild_id, &slug)? {
        let linked = existing
            .discord_id
            .as_deref()
            .is_some_and(|id| !id.is_empty());
        if existing.kind == "event" && !linked {
            return Ok(existing.id);
        }
        return Err(OpError::message(format!("#{slug} already exists.")));
    }
    let position = schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .select(diesel::dsl::max(schema::channels::position))
        .first::<Option<i64>>(conn)?
        .unwrap_or(0)
        + 1;
    let id = diesel::insert_into(schema::channels::table)
        .values(&NewChannel {
            guild_id,
            name: &slug,
            kind: "event",
            position,
        })
        .returning(schema::channels::id)
        .get_result(conn)?;
    Ok(id)
}

pub fn bind_channel_discord(
    conn: &mut Connection,
    channel_id: i64,
    discord_id: &str,
) -> Result<String, OpError> {
    let channel = store::channel(conn, channel_id)?.ok_or(OpError::NotFound)?;
    diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel_id)))
        .set(schema::channels::discord_id.eq(discord_id))
        .execute(conn)?;
    let notice = format!("Created #{}.", channel.name);
    log(conn, channel.guild_id, None, &notice)?;
    Ok(notice)
}

pub fn discard_channel(conn: &mut Connection, channel_id: i64) -> Result<(), OpError> {
    diesel::delete(schema::channels::table.filter(schema::channels::id.eq(channel_id)))
        .execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::channels)]
struct NewChannel<'a> {
    guild_id: i64,
    name: &'a str,
    kind: &'a str,
    position: i64,
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
    let actor = must_membership(conn, actor_id)?;
    if !actor.organizer && actor.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    let settings = store::settings(conn, guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
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
    let json = serde_json::to_value(body).map_err(|err| OpError::message(err.to_string()))?;
    let id = diesel::insert_into(schema::presets::table)
        .values(&NewPreset {
            public_id: crate::ids::preset_id(),
            guild_id,
            name,
            body: json,
        })
        .returning(schema::presets::id)
        .get_result(conn)?;
    Ok(id)
}

#[derive(Insertable)]
#[diesel(table_name = schema::presets)]
struct NewPreset<'a> {
    public_id: uuid::Uuid,
    guild_id: i64,
    name: &'a str,
    body: serde_json::Value,
}

pub fn delete_preset(conn: &mut Connection, preset_id: i64, actor_id: i64) -> Result<(), OpError> {
    let preset = store::preset(conn, preset_id)?.ok_or(OpError::NotFound)?;
    let actor = must_membership(conn, actor_id)?;
    if actor.guild_id != preset.guild_id {
        return Err(OpError::NotFound);
    }
    if !actor.organizer {
        let settings = store::settings(conn, preset.guild_id)?;
        let roles = store::membership_role_ids(conn, &actor)?;
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
        .values(&NewLibraryOption {
            guild_id,
            name,
            emoji,
        })
        .on_conflict((
            schema::option_library::guild_id,
            schema::option_library::name,
        ))
        .do_update()
        .set(schema::option_library::emoji.eq(excluded(schema::option_library::emoji)))
        .execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::option_library)]
struct NewLibraryOption<'a> {
    guild_id: i64,
    name: &'a str,
    emoji: &'a str,
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
    let actor = must_membership(conn, actor_id)?;
    if actor.guild_id != guild_id {
        return Err(OpError::NotFound);
    }
    if actor.organizer {
        return Ok(());
    }
    let settings = store::settings(conn, guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
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
    let actor = must_membership(conn, actor_id)?;
    let settings = store::settings(conn, event.guild_id)?;
    let roles = store::membership_role_ids(conn, &actor)?;
    if !can_edit(&actor, &event, &settings, &roles) {
        return Err(OpError::message("You can't sync this event."));
    }
    if !matches!(event.status.as_str(), "posted" | "scheduled") {
        return Err(OpError::message(
            "Post or schedule the event before syncing it.",
        ));
    }
    let stamp = Utc::now();
    diesel::update(schema::events::table.filter(schema::events::id.eq(event.id)))
        .set(schema::events::calendar_synced_at.eq(stamp))
        .execute(conn)?;
    let notice = format!("Marked {} for the calendar feed.", event.title);
    log(conn, event.guild_id, Some(event.id), &notice)?;
    Ok(notice)
}

#[derive(Insertable)]
#[diesel(table_name = schema::command_log)]
struct NewCommand<'a> {
    guild_id: i64,
    membership_id: i64,
    line: &'a str,
    reply: &'a str,
    created_at: DateTime<Utc>,
}

pub fn record_command(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    line: &str,
    reply: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::command_log::table)
        .values(&NewCommand {
            guild_id,
            membership_id,
            line,
            reply,
            created_at: Utc::now(),
        })
        .execute(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection;

    use super::*;

    #[test]
    fn an_event_channel_keeps_its_discord_id() {
        let mut conn = store::open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'Guild', 'UTC', '4242');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, organizer)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true FROM users WHERE discord_id = 'nell';
            ",
        )
        .unwrap();
        store::insert_default_settings(&mut conn, 1).unwrap();
        let id = create_channel(&mut conn, 1, 1, "This is an event channel").unwrap();
        let channel = store::channel(&mut conn, id).unwrap().unwrap();
        assert_eq!(channel.name, "this-is-an-event-channel");
        assert!(channel.discord_id.is_none());
        let same = create_channel(&mut conn, 1, 1, "#This is an event channel").unwrap();
        assert_eq!(same, id);
        let notice = bind_channel_discord(&mut conn, id, "900").unwrap();
        assert_eq!(notice, "Created #this-is-an-event-channel.");
        let again = create_channel(&mut conn, 1, 1, "this-is-an-event-channel").unwrap_err();
        assert_eq!(again.to_string(), "#this-is-an-event-channel already exists.");
        let channel = store::channel(&mut conn, id).unwrap().unwrap();
        assert_eq!(channel.discord_id.as_deref(), Some("900"));
        assert_eq!(channel.kind, "event");
        discard_channel(&mut conn, id).unwrap();
        assert!(store::channel(&mut conn, id).unwrap().is_none());
    }
}
