use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use crate::error::OpError;
use crate::schema;
use crate::store::{self, Channel, Connection, Event, Membership};

pub(super) fn trim_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

pub(super) async fn role_names(conn: &mut Connection, ids: &[i64]) -> Result<Vec<String>, OpError> {
    let mut names = Vec::new();
    for id in ids {
        if let Some(role) = store::role(conn, *id).await? {
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

/// How long the same note about one event keeps folding into the last one.
const LOG_GROUPING: chrono::Duration = chrono::Duration::minutes(15);

/// Notes a change in the server's activity. The same note about the same event
/// within a short while, such as several quick edits, replaces the earlier one,
/// so the run reads as one line at the top with the latest time.
pub(super) async fn log(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    body: &str,
) -> Result<(), OpError> {
    let now = Utc::now();
    if let Some(event_id) = event_id {
        diesel::delete(
            schema::activity::table
                .filter(schema::activity::guild_id.eq(guild_id))
                .filter(schema::activity::event_id.eq(event_id))
                .filter(schema::activity::body.eq(body))
                .filter(schema::activity::created_at.gt(now - LOG_GROUPING)),
        )
        .execute(conn)
        .await?;
    }
    diesel::insert_into(schema::activity::table)
        .values(&NewActivity {
            guild_id,
            event_id,
            body,
            created_at: now,
        })
        .execute(conn)
        .await?;
    Ok(())
}

pub(super) async fn must_event(conn: &mut Connection, id: i64) -> Result<Event, OpError> {
    store::event(conn, id).await?.ok_or(OpError::NotFound)
}

/// The event, its row locked until the transaction ends, so that changes to its
/// signups take turns: each one counts places after the last one committed.
pub(super) async fn lock_event(conn: &mut Connection, id: i64) -> Result<Event, OpError> {
    Event::by_id(id)
        .for_update()
        .first(conn)
        .await
        .optional()?
        .ok_or(OpError::NotFound)
}

pub(super) async fn must_membership(conn: &mut Connection, id: i64) -> Result<Membership, OpError> {
    store::membership(conn, id).await?.ok_or(OpError::NotFound)
}

pub(super) async fn must_channel(conn: &mut Connection, id: i64) -> Result<Channel, OpError> {
    store::channel(conn, id).await?.ok_or(OpError::NotFound)
}

pub(super) async fn must_role(conn: &mut Connection, id: i64) -> Result<store::Role, OpError> {
    store::role(conn, id).await?.ok_or(OpError::NotFound)
}

pub(super) async fn must_option(
    conn: &mut Connection,
    id: i64,
) -> Result<store::SignupOption, OpError> {
    store::option(conn, id).await?.ok_or(OpError::NotFound)
}

pub(super) async fn must_series(conn: &mut Connection, id: i64) -> Result<store::Series, OpError> {
    store::series(conn, id).await?.ok_or(OpError::NotFound)
}

pub(super) async fn must_guild(conn: &mut Connection, id: i64) -> Result<store::Guild, OpError> {
    store::guild(conn, id).await?.ok_or(OpError::NotFound)
}
