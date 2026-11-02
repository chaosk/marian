use std::collections::{HashMap, HashSet};

use diesel::prelude::*;

use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Event, Membership, Settings};

/// Whether someone with `roles` holds any of the roles a permission names.
pub fn holds_any(allowed: &[i64], roles: &[i64]) -> bool {
    allowed.iter().any(|role| roles.contains(role))
}

pub fn can_create(user: &Membership, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || settings.role_event.is_empty() || holds_any(&settings.role_event, roles)
}

pub fn can_edit(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || event.created_by == Some(user.id) || holds_any(&settings.role_edit, roles)
}

pub fn can_delete(user: &Membership, event: &Event, settings: &Settings, roles: &[i64]) -> bool {
    user.server_manager || event.created_by == Some(user.id) || holds_any(&settings.role_delete, roles)
}

const ADMINISTRATOR: u64 = 1 << 3;
const VIEW_CHANNEL: u64 = 1 << 10;

/// A Discord channel's permission overwrite for one role or member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overwrite {
    /// For a member; otherwise for a role, @everyone's id being the server's.
    pub member: bool,
    pub target: String,
    pub allow: u64,
    pub deny: u64,
}

/// Whether Discord shows a channel to a member whose server-wide permissions
/// are `base` (@everyone's and their roles' together): @everyone's overwrite
/// first, then their roles' together, then their own.
pub fn sees_channel(base: u64, everyone: &str, member: &str, roles: &[String], overwrites: &[Overwrite]) -> bool {
    if base & ADMINISTRATOR != 0 {
        return true;
    }
    let apply = |perms: u64, allow: u64, deny: u64| (perms & !deny) | allow;
    let mut perms = base;
    if let Some(all) = overwrites.iter().find(|o| !o.member && o.target == everyone) {
        perms = apply(perms, all.allow, all.deny);
    }
    let (allow, deny) = overwrites
        .iter()
        .filter(|o| !o.member && o.target != everyone && roles.contains(&o.target))
        .fold((0, 0), |(allow, deny), o| (allow | o.allow, deny | o.deny));
    perms = apply(perms, allow, deny);
    if let Some(own) = overwrites.iter().find(|o| o.member && o.target == member) {
        perms = apply(perms, own.allow, own.deny);
    }
    perms & VIEW_CHANNEL != 0
}

/// The channels a member can't see in Discord, so Marian doesn't show them
/// those channels' events either. Server managers see everything, and anyone
/// sees the events they made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sight {
    member_id: i64,
    hidden: HashSet<i64>,
}

impl Sight {
    pub fn sees_channel(&self, channel_id: i64) -> bool {
        !self.hidden.contains(&channel_id)
    }

    pub fn sees_event(&self, event: &Event) -> bool {
        event.created_by == Some(self.member_id) || self.sees_channel(event.channel_id)
    }

    /// The hidden channels, for filtering in a query.
    pub fn hidden_channels(&self) -> Vec<i64> {
        self.hidden.iter().copied().collect()
    }
}

/// What `member` can see, from the permissions Discord last sent. Until the
/// bot has sent the server's permissions, every channel is shown.
pub fn sight(conn: &mut Connection, member: &Membership) -> Result<Sight, OpError> {
    if member.server_manager {
        return Ok(Sight {
            member_id: member.id,
            hidden: HashSet::new(),
        });
    }
    let member_discord: String = schema::users::table
        .find(member.user_id)
        .select(schema::users::discord_id)
        .first(conn)?;
    let held: Vec<(Option<String>, i64)> = schema::membership_roles::table
        .inner_join(schema::roles::table)
        .filter(schema::membership_roles::membership_id.eq(member.id))
        .filter(schema::roles::gone_at.is_null())
        .select((schema::roles::discord_id, schema::roles::permissions))
        .load(conn)?;
    sight_of(conn, member.guild_id, member.id, &member_discord, held)
}

/// What a Discord member can see. Someone Marian doesn't know yet sees only
/// what @everyone does, besides their own overwrites.
pub fn sight_for_discord(conn: &mut Connection, guild_id: i64, discord_user: &str) -> Result<Sight, OpError> {
    match store::membership_by_discord(conn, guild_id, discord_user)? {
        Some(member) => sight(conn, &member),
        None => sight_of(conn, guild_id, 0, discord_user, Vec::new()),
    }
}

/// What a member holding `held` roles, as (Discord id, permissions), can see.
fn sight_of(
    conn: &mut Connection,
    guild_id: i64,
    member_id: i64,
    member_discord: &str,
    held: Vec<(Option<String>, i64)>,
) -> Result<Sight, OpError> {
    let mut sight = Sight {
        member_id,
        hidden: HashSet::new(),
    };
    let (guild_discord, everyone): (Option<String>, Option<i64>) = schema::guilds::table
        .find(guild_id)
        .select((schema::guilds::discord_id, schema::guilds::everyone_permissions))
        .first(conn)?;
    let (Some(guild_discord), Some(everyone)) = (guild_discord, everyone) else {
        return Ok(sight);
    };
    let base = held.iter().fold(everyone as u64, |base, (_, perms)| base | *perms as u64);
    if base & ADMINISTRATOR != 0 {
        return Ok(sight);
    }
    let roles: Vec<String> = held.into_iter().filter_map(|(id, _)| id).collect();
    let channels: Vec<i64> = schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .filter(schema::channels::discord_id.is_not_null())
        .filter(schema::channels::discord_id.ne(""))
        .select(schema::channels::id)
        .load(conn)?;
    let rows: Vec<(i64, String, String, i64, i64)> = schema::channel_overwrites::table
        .inner_join(schema::channels::table)
        .filter(schema::channels::guild_id.eq(guild_id))
        .select((
            schema::channel_overwrites::channel_id,
            schema::channel_overwrites::kind,
            schema::channel_overwrites::target,
            schema::channel_overwrites::allow,
            schema::channel_overwrites::deny,
        ))
        .load(conn)?;
    let mut overwrites: HashMap<i64, Vec<Overwrite>> = HashMap::new();
    for (channel_id, kind, target, allow, deny) in rows {
        overwrites.entry(channel_id).or_default().push(Overwrite {
            member: kind == "member",
            target,
            allow: allow as u64,
            deny: deny as u64,
        });
    }
    for channel_id in channels {
        let these = overwrites.get(&channel_id).map(Vec::as_slice).unwrap_or(&[]);
        if !sees_channel(base, &guild_discord, member_discord, &roles, these) {
            sight.hidden.insert(channel_id);
        }
    }
    Ok(sight)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEND: u64 = 1 << 11;

    fn role(target: &str, allow: u64, deny: u64) -> Overwrite {
        Overwrite { member: false, target: target.into(), allow, deny }
    }

    fn member(target: &str, allow: u64, deny: u64) -> Overwrite {
        Overwrite { member: true, target: target.into(), allow, deny }
    }

    #[test]
    fn a_channel_without_overwrites_follows_the_server() {
        assert!(sees_channel(VIEW_CHANNEL, "1", "9", &[], &[]));
        assert!(!sees_channel(SEND, "1", "9", &[], &[]));
    }

    #[test]
    fn a_private_channel_shows_only_to_the_roles_it_lets_in() {
        let private = [role("1", 0, VIEW_CHANNEL), role("5", VIEW_CHANNEL, 0)];
        assert!(!sees_channel(VIEW_CHANNEL, "1", "9", &[], &private));
        assert!(!sees_channel(VIEW_CHANNEL, "1", "9", &["6".into()], &private));
        assert!(sees_channel(VIEW_CHANNEL, "1", "9", &["5".into()], &private));
    }

    #[test]
    fn one_role_letting_in_beats_another_keeping_out() {
        let split = [role("5", VIEW_CHANNEL, 0), role("6", 0, VIEW_CHANNEL)];
        assert!(sees_channel(VIEW_CHANNEL, "1", "9", &["5".into(), "6".into()], &split));
    }

    #[test]
    fn the_members_own_overwrite_comes_last() {
        let kept_out = [role("5", VIEW_CHANNEL, 0), member("9", 0, VIEW_CHANNEL)];
        assert!(!sees_channel(VIEW_CHANNEL, "1", "9", &["5".into()], &kept_out));
        let let_in = [role("1", 0, VIEW_CHANNEL), member("9", VIEW_CHANNEL, 0)];
        assert!(sees_channel(VIEW_CHANNEL, "1", "9", &[], &let_in));
        assert!(!sees_channel(VIEW_CHANNEL, "1", "8", &[], &let_in));
    }

    #[test]
    fn administrators_see_every_channel() {
        let private = [role("1", 0, VIEW_CHANNEL), member("9", 0, VIEW_CHANNEL)];
        assert!(sees_channel(ADMINISTRATOR, "1", "9", &[], &private));
    }
}
