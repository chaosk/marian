use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::access::{can_create, can_edit, holds_any};
use super::db::{log, must_event, must_membership};
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection};

pub fn set_user_reminders(
    conn: &mut Connection,
    membership_id: i64,
    enabled: bool,
) -> Result<crate::i18n::Msg, OpError> {
    let user = must_membership(conn, membership_id)?;
    diesel::update(schema::memberships::table.filter(schema::memberships::id.eq(membership_id)))
        .set(schema::memberships::reminders_enabled.eq(enabled))
        .execute(conn)?;
    Ok(if enabled {
        crate::msg!("notice-reminders-on", name = &user.name)
    } else {
        crate::msg!("notice-reminders-off", name = &user.name)
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
    let allowed = actor.server_manager
        || if settings.role_channel.is_empty() {
            can_create(&actor, &settings, &roles)
        } else {
            holds_any(&settings.role_channel, &roles)
        };
    if !allowed {
        return Err(OpError::from(crate::msg!("err-cant-create-channels")));
    }
    let slug = slug_channel(name);
    if slug.is_empty() {
        return Err(OpError::from(crate::msg!("err-channel-name")));
    }
    if let Some(existing) = store::channel_by_name(conn, guild_id, &slug)? {
        let linked = existing
            .discord_id
            .as_deref()
            .is_some_and(|id| !id.is_empty());
        if existing.kind == "event" && !linked {
            return Ok(existing.id);
        }
        return Err(OpError::from(crate::msg!("err-channel-exists", name = slug.as_str())));
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
) -> Result<crate::i18n::Msg, OpError> {
    let channel = store::channel(conn, channel_id)?.ok_or(OpError::NotFound)?;
    diesel::update(schema::channels::table.filter(schema::channels::id.eq(channel_id)))
        .set(schema::channels::discord_id.eq(discord_id))
        .execute(conn)?;
    let notice = crate::msg!("notice-channel-created", name = &channel.name);
    log(conn, channel.guild_id, None, &notice.to_string())?;
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

/// A channel's name as Discord writes it: lower case, with a dash for each run
/// of spaces and ASCII punctuation. Letters like "ę", and emoji, stay.
pub fn slug_channel(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.trim().trim_start_matches('#').chars() {
        if ch.is_alphanumeric() || !(ch.is_ascii() || ch.is_whitespace()) {
            out.extend(ch.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').chars().take(40).collect()
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
        return Err(OpError::from(crate::msg!("err-cant-sync")));
    }
    if !matches!(event.status.as_str(), "posted" | "scheduled") {
        return Err(OpError::from(crate::msg!("err-sync-unposted")));
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
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, server_manager)
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
        assert_eq!(notice.to_string(), "Created #this-is-an-event-channel.");
        let again = create_channel(&mut conn, 1, 1, "this-is-an-event-channel").unwrap_err();
        assert_eq!(again.to_string(), "#this-is-an-event-channel already exists.");
        let channel = store::channel(&mut conn, id).unwrap().unwrap();
        assert_eq!(channel.discord_id.as_deref(), Some("900"));
        assert_eq!(channel.kind, "event");
        discard_channel(&mut conn, id).unwrap();
        assert!(store::channel(&mut conn, id).unwrap().is_none());
    }
}
