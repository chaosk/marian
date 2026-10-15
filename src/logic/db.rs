use diesel::prelude::*;

use crate::error::OpError;
use crate::schema;
use crate::store::{self, Channel, Connection, Event, User};

pub(super) fn trim_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

pub(super) fn role_names(conn: &mut Connection, ids: &[i64]) -> Result<Vec<String>, OpError> {
    let mut names = Vec::new();
    for id in ids {
        if let Some(role) = store::role(conn, *id)? {
            names.push(role.name);
        }
    }
    Ok(names)
}

pub(super) fn log(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    body: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::activity::table)
        .values((
            schema::activity::guild_id.eq(guild_id),
            schema::activity::event_id.eq(event_id),
            schema::activity::body.eq(body),
            schema::activity::created_at.eq(store::now_stamp()),
        ))
        .execute(conn)?;
    Ok(())
}

pub(super) fn must_event(conn: &mut Connection, id: i64) -> Result<Event, OpError> {
    store::event(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_user(conn: &mut Connection, id: i64) -> Result<User, OpError> {
    store::user(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_channel(conn: &mut Connection, id: i64) -> Result<Channel, OpError> {
    store::channel(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_role(conn: &mut Connection, id: i64) -> Result<store::Role, OpError> {
    store::role(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_option(conn: &mut Connection, id: i64) -> Result<store::SignupOption, OpError> {
    store::option(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_series(conn: &mut Connection, id: i64) -> Result<store::Series, OpError> {
    store::series(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_guild(conn: &mut Connection, id: i64) -> Result<store::Guild, OpError> {
    store::guild(conn, id)?.ok_or(OpError::NotFound)
}
