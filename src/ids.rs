//! Public identifiers shown in links. Integer keys stay inside the database.
//!
//! Event links use UUID version 7: a millisecond timestamp, then random bits.
//! A failed system random source stops the call instead of leaving those bits zero.

use uuid::Uuid;

pub fn routine_id() -> Uuid {
    event_id()
}

pub fn series_id() -> Uuid {
    event_id()
}

pub fn preset_id() -> Uuid {
    event_id()
}

pub fn event_id() -> Uuid {
    Uuid::now_v7()
}

pub fn parse_event_id(token: &str) -> Option<Uuid> {
    let end = token
        .find(|ch: char| !ch.is_ascii_hexdigit() && ch != '-')
        .unwrap_or(token.len());
    Uuid::parse_str(&token[..end]).ok()
}

/// Channels, roles, members, and signup options carry a `public_id` too. The
/// API names them only by that. These turn it into the row's key and back,
/// always within one server or event, so an id from elsewhere is not found.
pub mod public {
    use diesel::prelude::*;
    use std::collections::HashMap;
    use uuid::Uuid;

    use crate::error::OpError;
    use crate::schema;
    use crate::store::Connection;

    fn parse(public: &str) -> Option<Uuid> {
        Uuid::parse_str(public.trim()).ok()
    }

    pub fn channel(conn: &mut Connection, id: i64) -> Result<String, OpError> {
        Ok(schema::channels::table
            .find(id)
            .select(schema::channels::public_id)
            .first::<Uuid>(conn)
            .optional()?
            .map(|id| id.to_string())
            .unwrap_or_default())
    }

    pub fn channel_id(conn: &mut Connection, guild_id: i64, public: &str) -> Result<i64, OpError> {
        let found = match parse(public) {
            Some(public) => schema::channels::table
                .filter(schema::channels::guild_id.eq(guild_id))
                .filter(schema::channels::public_id.eq(public))
                .select(schema::channels::id)
                .first::<i64>(conn)
                .optional()?,
            None => None,
        };
        found.ok_or_else(|| OpError::from(crate::msg!("err-pick-channel")))
    }

    /// Roles by key, as public ids in the same order. A role gone since is left out.
    pub fn roles(conn: &mut Connection, ids: &[i64]) -> Result<Vec<String>, OpError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let found: HashMap<i64, Uuid> = schema::roles::table
            .filter(schema::roles::id.eq_any(ids))
            .select((schema::roles::id, schema::roles::public_id))
            .load::<(i64, Uuid)>(conn)?
            .into_iter()
            .collect();
        Ok(ids.iter().filter_map(|id| found.get(id)).map(Uuid::to_string).collect())
    }

    pub fn role(conn: &mut Connection, id: Option<i64>) -> Result<Option<String>, OpError> {
        Ok(match id {
            Some(id) => roles(conn, &[id])?.into_iter().next(),
            None => None,
        })
    }

    /// Public role ids from a request, as keys. One that isn't a role in this
    /// server is refused rather than dropped.
    pub fn role_ids(conn: &mut Connection, guild_id: i64, public: &[String]) -> Result<Vec<i64>, OpError> {
        if public.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<Uuid> = public
            .iter()
            .map(|id| parse(id).ok_or_else(missing_role))
            .collect::<Result<_, _>>()?;
        let found: HashMap<Uuid, i64> = schema::roles::table
            .filter(schema::roles::guild_id.eq(guild_id))
            .filter(schema::roles::public_id.eq_any(&wanted))
            .select((schema::roles::public_id, schema::roles::id))
            .load::<(Uuid, i64)>(conn)?
            .into_iter()
            .collect();
        wanted
            .iter()
            .map(|id| found.get(id).copied().ok_or_else(missing_role))
            .collect()
    }

    pub fn role_id(conn: &mut Connection, guild_id: i64, public: Option<&str>) -> Result<Option<i64>, OpError> {
        match public.filter(|id| !id.trim().is_empty()) {
            Some(id) => Ok(role_ids(conn, guild_id, &[id.to_string()])?.into_iter().next()),
            None => Ok(None),
        }
    }

    fn missing_role() -> OpError {
        OpError::from(crate::msg!("err-role-elsewhere"))
    }

    pub fn member_id(conn: &mut Connection, guild_id: i64, public: &str) -> Result<i64, OpError> {
        let Some(public) = parse(public) else {
            return Err(OpError::NotFound);
        };
        schema::memberships::table
            .filter(schema::memberships::guild_id.eq(guild_id))
            .filter(schema::memberships::public_id.eq(public))
            .select(schema::memberships::id)
            .first::<i64>(conn)
            .optional()?
            .ok_or(OpError::NotFound)
    }

    /// A signup option on this event, by its public id.
    pub fn option_id(conn: &mut Connection, event_id: i64, public: &str) -> Result<i64, OpError> {
        let Some(public) = parse(public) else {
            return Err(OpError::NotFound);
        };
        schema::signup_options::table
            .filter(schema::signup_options::event_id.eq(event_id))
            .filter(schema::signup_options::public_id.eq(public))
            .select(schema::signup_options::id)
            .first::<i64>(conn)
            .optional()?
            .ok_or(OpError::NotFound)
    }

    /// The keys of an event's options, by public id, for a list sent back to save.
    pub fn option_ids(conn: &mut Connection, event_id: i64) -> Result<HashMap<String, i64>, OpError> {
        Ok(schema::signup_options::table
            .filter(schema::signup_options::event_id.eq(event_id))
            .select((schema::signup_options::public_id, schema::signup_options::id))
            .load::<(Uuid, i64)>(conn)?
            .into_iter()
            .map(|(public, id)| (public.to_string(), id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_id_is_a_version_7_uuid() {
        let id = event_id();
        assert_eq!(id.get_version(), Some(uuid::Version::SortRand));
        assert_eq!(parse_event_id(&id.to_string()), Some(id));
        assert_ne!(event_id(), id);
    }
}
