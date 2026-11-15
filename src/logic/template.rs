//! Shared event wording. A series or a routine keeps one template, and each
//! event it makes copies the wording as written.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::event::{insert_event_roles, replace_children};
use super::model::{OptionInput, ReminderInput, SeriesTemplate};
use crate::error::OpError;
use crate::schema;
use crate::store::{Connection, Series, StoredTemplate};
use crate::values::EventStatus;

#[derive(Debug, Clone)]
pub struct EventTemplate {
    pub guild_id: i64,
    pub channel_id: i64,
    pub title: String,
    pub description: String,
    pub location: String,
    pub duration_minutes: i64,
    pub color: String,
    pub image_url: Option<String>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub mention_everyone: bool,
    pub attendee_role_id: Option<i64>,
    pub cutoff_offset_minutes: Option<i64>,
    pub options: Vec<OptionInput>,
    pub reminders: Vec<ReminderInput>,
    pub mention_role_ids: Vec<i64>,
    pub restrict_role_ids: Vec<i64>,
}

pub struct Spawn {
    pub created_by: Option<i64>,
    /// Who picked the template's attendee role.
    pub attendee_role_by: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub post_at: Option<DateTime<Utc>>,
    pub status: EventStatus,
    pub series_id: Option<i64>,
    pub occurrence_index: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = schema::event_templates)]
struct NewTemplate<'a> {
    guild_id: i64,
    channel_id: i64,
    title: &'a str,
    description: &'a str,
    location: &'a str,
    duration_minutes: i64,
    color: &'a str,
    image_url: Option<&'a str>,
    thread_enabled: bool,
    multiple_signups: bool,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    cutoff_offset_minutes: Option<i64>,
    reminders_json: serde_json::Value,
    mention_role_ids: &'a [i64],
    restrict_role_ids: &'a [i64],
    created_at: DateTime<Utc>,
}

pub async fn insert(conn: &mut Connection, template: &EventTemplate) -> Result<i64, OpError> {
    let id = diesel::insert_into(schema::event_templates::table)
        .values(&NewTemplate {
            guild_id: template.guild_id,
            channel_id: template.channel_id,
            title: &template.title,
            description: &template.description,
            location: &template.location,
            duration_minutes: template.duration_minutes,
            color: &template.color,
            image_url: template.image_url.as_deref(),
            thread_enabled: template.thread_enabled,
            multiple_signups: template.multiple_signups,
            mention_everyone: template.mention_everyone,
            attendee_role_id: template.attendee_role_id,
            cutoff_offset_minutes: template.cutoff_offset_minutes,
            reminders_json: to_json(&template.reminders),
            mention_role_ids: &template.mention_role_ids,
            restrict_role_ids: &template.restrict_role_ids,
            created_at: Utc::now(),
        })
        .returning(schema::event_templates::id)
        .get_result(conn)
        .await?;
    write_types(conn, id, &template.options).await?;
    Ok(id)
}

/// A template's attendance types, in order, replacing what it had.
async fn write_types(
    conn: &mut Connection,
    template_id: i64,
    options: &[OptionInput],
) -> Result<(), OpError> {
    diesel::delete(
        schema::template_options::table
            .filter(schema::template_options::template_id.eq(template_id)),
    )
    .execute(conn)
    .await?;
    let rows: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(position, opt)| {
            (
                schema::template_options::template_id.eq(template_id),
                schema::template_options::type_id.eq(opt.type_id),
                schema::template_options::position.eq(position as i64),
                schema::template_options::capacity.eq(opt.capacity),
                schema::template_options::role_id.eq(opt.role_id),
                schema::template_options::max_guests.eq(opt.max_guests),
            )
        })
        .collect();
    diesel::insert_into(schema::template_options::table)
        .values(&rows)
        .execute(conn)
        .await?;
    Ok(())
}

async fn read_types(conn: &mut Connection, template_id: i64) -> Result<Vec<OptionInput>, OpError> {
    let rows: Vec<super::library::ListedType> = schema::template_options::table
        .filter(schema::template_options::template_id.eq(template_id))
        .order(schema::template_options::position.asc())
        .select((
            schema::template_options::type_id,
            schema::template_options::capacity,
            schema::template_options::role_id,
            schema::template_options::max_guests,
        ))
        .load(conn)
        .await?;
    Ok(rows.into_iter().map(super::library::listed).collect())
}

pub async fn update(
    conn: &mut Connection,
    id: i64,
    template: &EventTemplate,
) -> Result<(), OpError> {
    diesel::update(schema::event_templates::table.filter(schema::event_templates::id.eq(id)))
        .set((
            schema::event_templates::channel_id.eq(template.channel_id),
            schema::event_templates::title.eq(&template.title),
            schema::event_templates::description.eq(&template.description),
            schema::event_templates::location.eq(&template.location),
            schema::event_templates::duration_minutes.eq(template.duration_minutes),
            schema::event_templates::color.eq(&template.color),
            schema::event_templates::image_url.eq(template.image_url.as_deref()),
            schema::event_templates::thread_enabled.eq(template.thread_enabled),
            schema::event_templates::multiple_signups.eq(template.multiple_signups),
            schema::event_templates::mention_everyone.eq(template.mention_everyone),
            schema::event_templates::attendee_role_id.eq(template.attendee_role_id),
            schema::event_templates::cutoff_offset_minutes.eq(template.cutoff_offset_minutes),
            schema::event_templates::reminders_json.eq(to_json(&template.reminders)),
            schema::event_templates::mention_role_ids.eq(&template.mention_role_ids),
            schema::event_templates::restrict_role_ids.eq(&template.restrict_role_ids),
        ))
        .execute(conn)
        .await?;
    write_types(conn, id, &template.options).await?;
    Ok(())
}

pub async fn update_wording(
    conn: &mut Connection,
    id: i64,
    channel_id: i64,
    title: &str,
    description: &str,
    duration_minutes: i64,
) -> Result<(), OpError> {
    diesel::update(schema::event_templates::table.filter(schema::event_templates::id.eq(id)))
        .set((
            schema::event_templates::channel_id.eq(channel_id),
            schema::event_templates::title.eq(title),
            schema::event_templates::description.eq(description),
            schema::event_templates::duration_minutes.eq(duration_minutes),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

/// Who picked a template's attendee role.
pub async fn role_by(conn: &mut Connection, id: i64) -> Result<Option<i64>, OpError> {
    Ok(schema::event_templates::table
        .filter(schema::event_templates::id.eq(id))
        .select(schema::event_templates::attendee_role_by)
        .first(conn)
        .await
        .optional()?
        .flatten())
}

pub async fn set_role_by(conn: &mut Connection, id: i64, by: Option<i64>) -> Result<(), OpError> {
    diesel::update(schema::event_templates::table.filter(schema::event_templates::id.eq(id)))
        .set(schema::event_templates::attendee_role_by.eq(by))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn load(conn: &mut Connection, id: i64) -> Result<EventTemplate, OpError> {
    let row = StoredTemplate::query()
        .filter(schema::event_templates::id.eq(id))
        .first(conn)
        .await
        .optional()?
        .ok_or(OpError::NotFound)?;
    Ok(EventTemplate {
        guild_id: row.guild_id,
        channel_id: row.channel_id,
        title: row.title,
        description: row.description,
        location: row.location,
        duration_minutes: row.duration_minutes,
        color: row.color,
        image_url: row.image_url,
        thread_enabled: row.thread_enabled,
        multiple_signups: row.multiple_signups,
        mention_everyone: row.mention_everyone,
        attendee_role_id: row.attendee_role_id,
        cutoff_offset_minutes: row.cutoff_offset_minutes,
        options: read_types(conn, row.id).await?,
        reminders: serde_json::from_value(row.reminders_json).unwrap_or_default(),
        mention_role_ids: row.mention_role_ids,
        restrict_role_ids: row.restrict_role_ids,
    })
}

pub async fn series_template(
    conn: &mut Connection,
    series: &Series,
) -> Result<SeriesTemplate, OpError> {
    let template = load(conn, series.template_id).await?;
    Ok(as_series(&template))
}

pub fn from_series(guild_id: i64, template: &SeriesTemplate) -> EventTemplate {
    EventTemplate {
        guild_id,
        channel_id: template.channel_id,
        title: template.title.clone(),
        description: template.description.clone(),
        location: template.location.clone(),
        duration_minutes: template.duration_minutes,
        color: template.color.clone(),
        image_url: template.image_url.clone(),
        thread_enabled: template.thread_enabled,
        multiple_signups: template.multiple_signups,
        mention_everyone: template.mention_everyone,
        attendee_role_id: template.attendee_role_id,
        cutoff_offset_minutes: template.cutoff_offset_minutes,
        options: template.options.clone(),
        reminders: template.reminders.clone(),
        mention_role_ids: template.mention_role_ids.clone(),
        restrict_role_ids: template.restrict_role_ids.clone(),
    }
}

pub fn as_series(template: &EventTemplate) -> SeriesTemplate {
    SeriesTemplate {
        title: template.title.clone(),
        description: template.description.clone(),
        location: template.location.clone(),
        duration_minutes: template.duration_minutes,
        channel_id: template.channel_id,
        color: template.color.clone(),
        image_url: template.image_url.clone(),
        thread_enabled: template.thread_enabled,
        multiple_signups: template.multiple_signups,
        mention_everyone: template.mention_everyone,
        attendee_role_id: template.attendee_role_id,
        mention_role_ids: template.mention_role_ids.clone(),
        restrict_role_ids: template.restrict_role_ids.clone(),
        cutoff_offset_minutes: template.cutoff_offset_minutes,
        options: template.options.clone(),
        reminders: template.reminders.clone(),
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::events)]
struct NewSpawnedEvent<'a> {
    public_id: uuid::Uuid,
    guild_id: i64,
    channel_id: i64,
    series_id: Option<i64>,
    occurrence_index: Option<i64>,
    title: &'a str,
    description: &'a str,
    location: &'a str,
    starts_at: DateTime<Utc>,
    duration_minutes: i64,
    status: EventStatus,
    post_at: Option<DateTime<Utc>>,
    color: &'a str,
    image_url: Option<&'a str>,
    cutoff_at: Option<DateTime<Utc>>,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    attendee_role_by: Option<i64>,
    thread_enabled: bool,
    multiple_signups: bool,
    created_by: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub async fn spawn(
    conn: &mut Connection,
    template: &EventTemplate,
    spec: &Spawn,
) -> Result<i64, OpError> {
    let stamp = Utc::now();
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| spec.starts_at - Duration::minutes(mins));
    let id = diesel::insert_into(schema::events::table)
        .values(&NewSpawnedEvent {
            public_id: crate::ids::event_id(),
            guild_id: template.guild_id,
            channel_id: template.channel_id,
            series_id: spec.series_id,
            occurrence_index: spec.occurrence_index,
            title: &template.title,
            description: &template.description,
            location: &template.location,
            starts_at: spec.starts_at,
            duration_minutes: template.duration_minutes,
            status: spec.status,
            post_at: spec.post_at,
            color: &template.color,
            image_url: template.image_url.as_deref(),
            cutoff_at: cutoff,
            mention_everyone: template.mention_everyone,
            attendee_role_id: template.attendee_role_id,
            attendee_role_by: spec.attendee_role_by,
            thread_enabled: template.thread_enabled,
            multiple_signups: template.multiple_signups,
            created_by: spec.created_by,
            created_at: stamp,
            updated_at: stamp,
        })
        .returning(schema::events::id)
        .get_result(conn)
        .await?;
    replace_children(conn, id, &template.options, &template.reminders).await?;
    insert_event_roles(
        conn,
        id,
        &template.mention_role_ids,
        &template.restrict_role_ids,
    )
    .await?;
    Ok(id)
}

fn to_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!([]))
}
