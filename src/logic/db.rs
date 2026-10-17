use chrono::{DateTime, Utc};
use diesel::prelude::*;

use crate::error::OpError;
use crate::schema;
use crate::store::{self, Channel, Connection, Event, Membership};

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

#[derive(Insertable)]
#[diesel(table_name = schema::activity)]
struct NewActivity<'a> {
    guild_id: i64,
    event_id: Option<i64>,
    body: &'a str,
    created_at: DateTime<Utc>,
}

pub(super) fn log(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    body: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::activity::table)
        .values(&NewActivity {
            guild_id,
            event_id,
            body,
            created_at: Utc::now(),
        })
        .execute(conn)?;
    Ok(())
}

pub(super) fn must_event(conn: &mut Connection, id: i64) -> Result<Event, OpError> {
    store::event(conn, id)?.ok_or(OpError::NotFound)
}

pub(super) fn must_membership(conn: &mut Connection, id: i64) -> Result<Membership, OpError> {
    store::membership(conn, id)?.ok_or(OpError::NotFound)
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
