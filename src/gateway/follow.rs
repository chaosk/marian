//! Keeps Marian's copy of servers, channels, roles and members as Discord has
//! them.

use std::collections::HashMap;
use std::sync::Arc;

use diesel::prelude::*;
use poise::serenity_prelude as serenity;
use tokio::sync::mpsc::UnboundedSender;

use crate::jobs::DiscordJob;
use crate::logic;
use crate::schema;
use crate::store::{self, Connection};

use super::{Data, Error};

pub(super) fn remember_guild(data: &Data, guild: &serenity::Guild) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let channels: Vec<(String, String)> = guild
        .channels
        .values()
        .filter(|channel| postable(channel.kind))
        .map(|channel| (channel.id.get().to_string(), channel.name.clone()))
        .collect();
    let roles: Vec<(String, String, String)> = guild
        .roles
        .values()
        .filter(|role| role.id.get() != guild.id.get())
        .map(|role| {
            (
                role.id.get().to_string(),
                role.name.clone(),
                role.colour.hex(),
            )
        })
        .collect();
    let guild_discord = guild.id.get().to_string();
    // All of it or none, so a failure halfway leaves no server with half its roles.
    store::with_tx(&mut conn, |tx| {
        let stored = logic::ensure_discord_guild(tx, &guild_discord, &guild.name)?;
        logic::sync_discord_channels(tx, stored.id, &channels)?;
        logic::sync_discord_roles(tx, stored.id, &roles)?;
        for role in guild.roles.values() {
            logic::sync_discord_permissions(
                tx,
                stored.id,
                &guild_discord,
                &role.id.get().to_string(),
                role.permissions.bits(),
            )?;
        }
        for channel in guild
            .channels
            .values()
            .filter(|channel| postable(channel.kind))
        {
            logic::sync_channel_overwrites(
                tx,
                stored.id,
                &channel.id.get().to_string(),
                &overwrites(channel),
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

/// A channel's permission overwrites, as Marian stores them.
fn overwrites(channel: &serenity::GuildChannel) -> Vec<logic::Overwrite> {
    channel
        .permission_overwrites
        .iter()
        .filter_map(|overwrite| {
            let (member, target) = match overwrite.kind {
                serenity::PermissionOverwriteType::Member(user) => (true, user.get()),
                serenity::PermissionOverwriteType::Role(role) => (false, role.get()),
                _ => return None,
            };
            Some(logic::Overwrite {
                member,
                target: target.to_string(),
                allow: overwrite.allow.bits(),
                deny: overwrite.deny.bits(),
            })
        })
        .collect()
}

fn stored_guild(
    conn: &mut Connection,
    guild: serenity::GuildId,
) -> Result<Option<store::Guild>, Error> {
    Ok(store::guild_by_discord(conn, &guild.get().to_string())?)
}

/// Follows a role Discord added or changed, and @everyone's permissions.
pub(super) fn follow_role(data: &Data, role: &serenity::Role) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, role.guild_id)? else {
        return Ok(());
    };
    if role.id.get() != role.guild_id.get() {
        logic::sync_discord_role(
            &mut conn,
            guild.id,
            &role.id.get().to_string(),
            &role.name,
            &role.colour.hex(),
        )?;
    }
    logic::sync_discord_permissions(
        &mut conn,
        guild.id,
        &role.guild_id.get().to_string(),
        &role.id.get().to_string(),
        role.permissions.bits(),
    )?;
    Ok(())
}

pub(super) fn role_deleted(
    data: &Data,
    guild: serenity::GuildId,
    role: serenity::RoleId,
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    logic::discord_role_gone(&mut conn, guild.id, &role.get().to_string())?;
    Ok(())
}

/// Brings the names, pictures and roles of members Marian knows up to date, as a chunk
/// of the server's member list has them, and redraws each post that changes once.
pub(super) fn follow_names<'a>(
    data: &Data,
    guild: serenity::GuildId,
    members: impl Iterator<Item = &'a serenity::Member>,
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(stored) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let mut events = std::collections::BTreeSet::new();
    for member in members {
        let user = member.user.id.get().to_string();
        if let Some((known, renamed)) =
            logic::follow_discord_name(&mut conn, stored.id, &user, &server_name(member))?
        {
            events.extend(renamed);
            logic::follow_discord_avatar(
                &mut conn,
                &known,
                server_avatar(guild, &member.user, member.avatar.as_ref()).as_deref(),
            )?;
            let roles: Vec<String> = member
                .roles
                .iter()
                .map(|role| role.get().to_string())
                .collect();
            store::replace_discord_roles(&mut conn, stored.id, known.id, &roles)?;
        }
    }
    redraw(&data.jobs, events.into_iter().collect());
    Ok(())
}

/// Redraws the posts that still show someone's old name.
pub(super) fn redraw(jobs: &UnboundedSender<DiscordJob>, events: Vec<i64>) {
    for event_id in events {
        let _ = jobs.send(DiscordJob::sync(event_id));
    }
}

/// The name a member goes by in their server: nickname first.
fn server_name(member: &serenity::Member) -> String {
    crate::auth::discord_display_name(
        member.nick.as_deref(),
        member.user.global_name.as_deref(),
        &member.user.name,
    )
}

/// The picture a member shows in their server: their server avatar first.
pub(super) fn server_avatar(
    guild: serenity::GuildId,
    user: &serenity::User,
    member_avatar: Option<&serenity::ImageHash>,
) -> Option<String> {
    let member_hash = member_avatar.map(ToString::to_string);
    let user_hash = user.avatar.as_ref().map(ToString::to_string);
    crate::auth::discord_avatar_url(
        &guild.get().to_string(),
        &user.id.get().to_string(),
        member_hash.as_deref(),
        user_hash.as_deref(),
    )
}

/// Keeps a member's name, picture and roles as Discord has them. Only members Marian
/// already knows are kept; the rest are picked up when they first use Marian.
pub(super) fn follow_member(
    data: &Data,
    guild: serenity::GuildId,
    user: &serenity::User,
    nick: Option<&str>,
    avatar: Option<&serenity::ImageHash>,
    roles: &[serenity::RoleId],
) -> Result<(), Error> {
    let avatar = server_avatar(guild, user, avatar);
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let name = crate::auth::discord_display_name(nick, user.global_name.as_deref(), &user.name);
    let Some((member, renamed)) =
        logic::follow_discord_name(&mut conn, guild.id, &user.id.get().to_string(), &name)?
    else {
        return Ok(());
    };
    redraw(&data.jobs, renamed);
    logic::follow_discord_avatar(&mut conn, &member, avatar.as_deref())?;
    let roles: Vec<String> = roles.iter().map(|role| role.get().to_string()).collect();
    store::replace_discord_roles(&mut conn, guild.id, member.id, &roles)?;
    Ok(())
}

/// Opens the server to the new member's web sessions.
pub(super) fn member_joined(data: &Data, member: &serenity::Member) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, member.guild_id)? else {
        return Ok(());
    };
    let user = member.user.id.get().to_string();
    let sessions = store::sessions_without_guild(&mut conn, guild.id, Some(&user))?;
    if sessions.is_empty() {
        return Ok(());
    }
    let (known, renamed) =
        logic::ensure_discord_user(&mut conn, guild.id, &user, &server_name(member))?;
    redraw(&data.jobs, renamed);
    let avatar = server_avatar(member.guild_id, &member.user, member.avatar.as_ref());
    logic::follow_discord_avatar(&mut conn, &known, avatar.as_deref())?;
    for (token, _, _) in &sessions {
        store::add_session_guild(&mut conn, token, guild.id)?;
    }
    Ok(())
}

/// Takes the server out of a departing member's web sessions and drops their roles in it.
pub(super) fn member_left(
    data: &Data,
    guild: serenity::GuildId,
    user: &serenity::User,
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = stored_guild(&mut conn, guild)? else {
        return Ok(());
    };
    let user = user.id.get().to_string();
    if let Some(member) = store::membership_by_discord(&mut conn, guild.id, &user)? {
        store::replace_discord_roles(&mut conn, guild.id, member.id, &[])?;
    }
    store::drop_session_guild(&mut conn, &user, guild.id)?;
    Ok(())
}

/// Opens a server to the signed-in members who are in it, so a server Marian
/// joins shows up in their server picker without signing in again. Each comes
/// in with their roles, and as a server manager if they own the server or
/// their roles (`roles`: each role's permissions, by Discord id) make them one.
pub(super) async fn admit_signed_in(
    db: store::Db,
    jobs: UnboundedSender<DiscordJob>,
    http: Arc<serenity::Http>,
    guild: serenity::GuildId,
    owner: serenity::UserId,
    roles: HashMap<u64, u64>,
) {
    let waiting = || -> Result<_, Error> {
        let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
        let Some(stored) = store::guild_by_discord(&mut conn, &guild.get().to_string())? else {
            return Ok(None);
        };
        Ok(Some((
            stored.id,
            store::sessions_without_guild(&mut conn, stored.id, None)?,
        )))
    };
    let (guild_id, sessions) = match waiting() {
        Ok(Some(found)) => found,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!("could not list sessions for {guild}: {err}");
            return;
        }
    };
    let mut members: Vec<(String, Vec<String>)> = Vec::new();
    for (token, user, _) in sessions {
        match members.last_mut() {
            Some((last, tokens)) if *last == user => tokens.push(token),
            _ => members.push((user, vec![token])),
        }
    }
    for (user, tokens) in members {
        let Ok(user_id) = user.parse::<u64>() else {
            continue;
        };
        let member = match http.get_member(guild, serenity::UserId::new(user_id)).await {
            Ok(member) => member,
            Err(serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(reply)))
                if reply.status_code == serenity::StatusCode::NOT_FOUND =>
            {
                continue;
            }
            Err(err) => {
                tracing::warn!("could not check membership in {guild}: {err}");
                continue;
            }
        };
        let held: Vec<u64> = member
            .roles
            .iter()
            .filter_map(|role| roles.get(&role.get()).copied())
            .collect();
        let everyone = roles.get(&guild.get()).copied().unwrap_or(0);
        let manages = crate::auth::manages_member(member.user.id == owner, everyone, &held);
        let role_ids: Vec<String> = member
            .roles
            .iter()
            .map(|role| role.get().to_string())
            .collect();
        let name = server_name(&member);
        let admitted = || -> Result<(), Error> {
            let mut conn = db.get().map_err(|_| "Could not reach the database.")?;
            let (membership, renamed) =
                logic::ensure_discord_user(&mut conn, guild_id, &user, &name)?;
            redraw(&jobs, renamed);
            logic::set_server_manager(&mut conn, membership.id, manages)?;
            store::replace_discord_roles(&mut conn, guild_id, membership.id, &role_ids)?;
            for token in &tokens {
                store::add_session_guild(&mut conn, token, guild_id)?;
            }
            Ok(())
        };
        if let Err(err) = admitted() {
            tracing::warn!("could not open {guild} to a signed-in member: {err}");
        }
    }
}

/// Channels Marian can post events in.
fn postable(kind: serenity::ChannelType) -> bool {
    matches!(
        kind,
        serenity::ChannelType::Text | serenity::ChannelType::News
    )
}

/// Follows a channel Discord added, renamed or deleted, so the channel lists
/// match Discord without waiting for a restart.
pub(super) fn follow_channel(
    data: &Data,
    channel: &serenity::GuildChannel,
    deleted: bool,
) -> Result<(), Error> {
    let mut conn = data.db.get().map_err(|_| "Could not reach the database.")?;
    let Some(guild) = store::guild_by_discord(&mut conn, &channel.guild_id.get().to_string())?
    else {
        return Ok(());
    };
    let discord_id = channel.id.get().to_string();
    if deleted || !postable(channel.kind) {
        logic::discord_channel_gone(&mut conn, guild.id, &discord_id)?;
    } else {
        logic::sync_discord_channel(&mut conn, guild.id, &discord_id, &channel.name)?;
        logic::sync_channel_overwrites(&mut conn, guild.id, &discord_id, &overwrites(channel))?;
    }
    Ok(())
}

pub(super) fn sync_roles(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    roles: &[(String, String, String)],
) -> Result<(), Error> {
    diesel::delete(
        schema::membership_roles::table
            .filter(schema::membership_roles::membership_id.eq(membership_id)),
    )
    .execute(conn)?;
    let mut role_ids = Vec::new();
    for (name, colour, discord_id) in roles {
        role_ids.push(logic::sync_discord_role(conn, guild_id, discord_id, name, colour)?.id);
    }
    store::insert_membership_roles(conn, membership_id, &role_ids)?;
    Ok(())
}
