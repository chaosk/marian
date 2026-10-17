//! Shared event wording. A series or a routine keeps one template.
//! Creating an event copies it and fills `{date}` with that event's date.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::must_guild;
use super::event::{insert_event_roles, replace_children};
use super::model::{OptionInput, ReminderInput, SeriesTemplate};
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{Connection, Series, StoredTemplate};

#[derive(Debug, Clone)]
pub struct EventTemplate {
    pub guild_id: i64,
    pub channel_id: i64,
    pub title: String,
    pub description: String,
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

pub struct Spawn<'a> {
    pub created_by: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub post_at: Option<DateTime<Utc>>,
    pub status: &'a str,
    pub series_id: Option<i64>,
    pub occurrence_index: Option<i64>,
    pub tz: Tz,
}

#[derive(Insertable)]
#[diesel(table_name = schema::event_templates)]
struct NewTemplate<'a> {
    guild_id: i64,
    channel_id: i64,
    title: &'a str,
    description: &'a str,
    duration_minutes: i64,
    color: &'a str,
    image_url: Option<&'a str>,
    thread_enabled: bool,
    multiple_signups: bool,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    cutoff_offset_minutes: Option<i64>,
    options_json: serde_json::Value,
    reminders_json: serde_json::Value,
    mention_role_ids: &'a [i64],
    restrict_role_ids: &'a [i64],
    created_at: DateTime<Utc>,
}

pub fn insert(conn: &mut Connection, template: &EventTemplate) -> Result<i64, OpError> {
    let id = diesel::insert_into(schema::event_templates::table)
        .values(&NewTemplate {
            guild_id: template.guild_id,
            channel_id: template.channel_id,
            title: &template.title,
            description: &template.description,
            duration_minutes: template.duration_minutes,
            color: &template.color,
            image_url: template.image_url.as_deref(),
            thread_enabled: template.thread_enabled,
            multiple_signups: template.multiple_signups,
            mention_everyone: template.mention_everyone,
            attendee_role_id: template.attendee_role_id,
            cutoff_offset_minutes: template.cutoff_offset_minutes,
            options_json: to_json(&template.options),
            reminders_json: to_json(&template.reminders),
            mention_role_ids: &template.mention_role_ids,
            restrict_role_ids: &template.restrict_role_ids,
            created_at: Utc::now(),
        })
        .returning(schema::event_templates::id)
        .get_result(conn)?;
    Ok(id)
}

pub fn update(conn: &mut Connection, id: i64, template: &EventTemplate) -> Result<(), OpError> {
    diesel::update(schema::event_templates::table.filter(schema::event_templates::id.eq(id)))
        .set((
            schema::event_templates::channel_id.eq(template.channel_id),
            schema::event_templates::title.eq(&template.title),
            schema::event_templates::description.eq(&template.description),
            schema::event_templates::duration_minutes.eq(template.duration_minutes),
            schema::event_templates::color.eq(&template.color),
            schema::event_templates::image_url.eq(template.image_url.as_deref()),
            schema::event_templates::thread_enabled.eq(template.thread_enabled),
            schema::event_templates::multiple_signups.eq(template.multiple_signups),
            schema::event_templates::mention_everyone.eq(template.mention_everyone),
            schema::event_templates::attendee_role_id.eq(template.attendee_role_id),
            schema::event_templates::cutoff_offset_minutes.eq(template.cutoff_offset_minutes),
            schema::event_templates::options_json.eq(to_json(&template.options)),
            schema::event_templates::reminders_json.eq(to_json(&template.reminders)),
            schema::event_templates::mention_role_ids.eq(&template.mention_role_ids),
            schema::event_templates::restrict_role_ids.eq(&template.restrict_role_ids),
        ))
        .execute(conn)?;
    Ok(())
}

pub fn update_wording(
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
        .execute(conn)?;
    Ok(())
}

pub fn load(conn: &mut Connection, id: i64) -> Result<EventTemplate, OpError> {
    let row = StoredTemplate::query()
        .filter(schema::event_templates::id.eq(id))
        .first(conn)
        .optional()?
        .ok_or(OpError::NotFound)?;
    Ok(EventTemplate {
        guild_id: row.guild_id,
        channel_id: row.channel_id,
        title: row.title,
        description: row.description,
        duration_minutes: row.duration_minutes,
        color: row.color,
        image_url: row.image_url,
        thread_enabled: row.thread_enabled,
        multiple_signups: row.multiple_signups,
        mention_everyone: row.mention_everyone,
        attendee_role_id: row.attendee_role_id,
        cutoff_offset_minutes: row.cutoff_offset_minutes,
        options: serde_json::from_value(row.options_json).unwrap_or_default(),
        reminders: serde_json::from_value(row.reminders_json).unwrap_or_default(),
        mention_role_ids: row.mention_role_ids,
        restrict_role_ids: row.restrict_role_ids,
    })
}

pub fn series_template(conn: &mut Connection, series: &Series) -> Result<SeriesTemplate, OpError> {
    let template = load(conn, series.template_id)?;
    Ok(as_series(&template))
}

pub fn from_series(guild_id: i64, template: &SeriesTemplate) -> EventTemplate {
    EventTemplate {
        guild_id,
        channel_id: template.channel_id,
        title: template.title.clone(),
        description: template.description.clone(),
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
    starts_at: DateTime<Utc>,
    duration_minutes: i64,
    status: &'a str,
    post_at: Option<DateTime<Utc>>,
    color: &'a str,
    image_url: Option<&'a str>,
    cutoff_at: Option<DateTime<Utc>>,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    thread_enabled: bool,
    multiple_signups: bool,
    created_by: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub fn spawn(
    conn: &mut Connection,
    template: &EventTemplate,
    spec: &Spawn<'_>,
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
            title: &fill(&template.title, spec.starts_at, spec.tz),
            description: &fill(&template.description, spec.starts_at, spec.tz),
            starts_at: spec.starts_at,
            duration_minutes: template.duration_minutes,
            status: spec.status,
            post_at: spec.post_at,
            color: &template.color,
            image_url: template.image_url.as_deref(),
            cutoff_at: cutoff,
            mention_everyone: template.mention_everyone,
            attendee_role_id: template.attendee_role_id,
            thread_enabled: template.thread_enabled,
            multiple_signups: template.multiple_signups,
            created_by: spec.created_by,
            created_at: stamp,
            updated_at: stamp,
        })
        .returning(schema::events::id)
        .get_result(conn)?;
    replace_children(conn, id, &template.options, &template.reminders)?;
    insert_event_roles(conn, id, &template.mention_role_ids, &template.restrict_role_ids)?;
    Ok(id)
}

pub fn fill_text(
    conn: &mut Connection,
    guild_id: i64,
    text: &str,
    start: DateTime<Utc>,
) -> Result<String, OpError> {
    let guild = must_guild(conn, guild_id)?;
    let tz = clock::zone(&guild.timezone)?;
    Ok(fill(text, start, tz))
}

pub fn fill(text: &str, start: DateTime<Utc>, tz: Tz) -> String {
    let date = clock::format_day(start.with_timezone(&tz).date_naive());
    text.replace("{date}", &date)
}

fn to_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!([]))
}
