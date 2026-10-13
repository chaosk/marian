use rusqlite::{params, Connection};

use crate::error::OpError;
use crate::store::{self, Channel, Event, User};

pub(super) fn trim_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

pub(super) fn role_names(conn: &Connection, ids: &[i64]) -> Result<Vec<String>, OpError> {
    let mut names = Vec::new();
    for id in ids {
        if let Some(role) = store::role(conn, *id)? {
            names.push(role.name);
        }
    }
    Ok(names)
}

pub(super) fn log(
    conn: &Connection,
    guild_id: i64,
    event_id: Option<i64>,
    body: &str,
) -> Result<(), OpError> {
    conn.execute(
        "INSERT INTO activity (guild_id, event_id, body, created_at) VALUES (?, ?, ?, ?)",
        params![guild_id, event_id, body, store::now_stamp()],
    )?;
    Ok(())
}

pub(super) fn must_event(conn: &Connection, id: i64) -> Result<Event, OpError> {
    store::event(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_user(conn: &Connection, id: i64) -> Result<User, OpError> {
    store::user(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_channel(conn: &Connection, id: i64) -> Result<Channel, OpError> {
    store::channel(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_role(conn: &Connection, id: i64) -> Result<store::Role, OpError> {
    store::role(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_option(conn: &Connection, id: i64) -> Result<store::SignupOption, OpError> {
    store::option(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_series(conn: &Connection, id: i64) -> Result<store::Series, OpError> {
    store::series(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_guild(conn: &Connection, id: i64) -> Result<store::Guild, OpError> {
    store::guild(conn, id)?.ok_or(OpError::NotFound)
}
