//! Shared event wording. A series or a routine keeps one template.
//! Creating an event copies it and fills `{date}` with that event's date.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use diesel::prelude::*;

use super::db::must_guild;
use super::event::replace_children;
use super::model::{OptionInput, ReminderInput, SeriesTemplate};
use crate::clock;
use crate::error::OpError;
use crate::schema;
use crate::store::{self, Connection, Series};

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
    pub created_by: i64,
    pub starts_at: DateTime<Utc>,
    pub post_at: Option<DateTime<Utc>>,
    pub status: &'a str,
    pub series_id: Option<i64>,
    pub occurrence_index: Option<i64>,
    pub tz: Tz,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::event_templates)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredTemplate {
    #[allow(dead_code)]
    id: i64,
    guild_id: i64,
    channel_id: i64,
    title: String,
    description: String,
    duration_minutes: i64,
    color: String,
    image_url: Option<String>,
    thread_enabled: bool,
    multiple_signups: bool,
    mention_everyone: bool,
    attendee_role_id: Option<i64>,
    cutoff_offset_minutes: Option<i64>,
    options_json: String,
    reminders_json: String,
    mention_role_ids: String,
    restrict_role_ids: String,
    #[allow(dead_code)]
    created_at: String,
}

pub fn insert(conn: &mut Connection, template: &EventTemplate) -> Result<i64, OpError> {
    let id = diesel::insert_into(schema::event_templates::table)
        .values((
            schema::event_templates::guild_id.eq(template.guild_id),
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
            schema::event_templates::mention_role_ids.eq(to_json(&template.mention_role_ids)),
            schema::event_templates::restrict_role_ids.eq(to_json(&template.restrict_role_ids)),
            schema::event_templates::created_at.eq(store::now_stamp()),
        ))
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
            schema::event_templates::mention_role_ids.eq(to_json(&template.mention_role_ids)),
            schema::event_templates::restrict_role_ids.eq(to_json(&template.restrict_role_ids)),
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
    let row = schema::event_templates::table
        .filter(schema::event_templates::id.eq(id))
        .select(StoredTemplate::as_select())
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
        options: serde_json::from_str(&row.options_json).unwrap_or_default(),
        reminders: serde_json::from_str(&row.reminders_json).unwrap_or_default(),
        mention_role_ids: serde_json::from_str(&row.mention_role_ids).unwrap_or_default(),
        restrict_role_ids: serde_json::from_str(&row.restrict_role_ids).unwrap_or_default(),
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

pub fn spawn(
    conn: &mut Connection,
    template: &EventTemplate,
    spec: &Spawn<'_>,
) -> Result<i64, OpError> {
    let stamp = store::now_stamp();
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| clock::stamp(spec.starts_at - Duration::minutes(mins)));
    let id = diesel::insert_into(schema::events::table)
        .values((
            schema::events::public_id.eq(crate::ids::event_id()),
            schema::events::guild_id.eq(template.guild_id),
            schema::events::channel_id.eq(template.channel_id),
            schema::events::series_id.eq(spec.series_id),
            schema::events::occurrence_index.eq(spec.occurrence_index),
            schema::events::title.eq(fill(&template.title, spec.starts_at, spec.tz)),
            schema::events::description.eq(fill(&template.description, spec.starts_at, spec.tz)),
            schema::events::starts_at.eq(clock::stamp(spec.starts_at)),
            schema::events::duration_minutes.eq(template.duration_minutes),
            schema::events::status.eq(spec.status),
            schema::events::post_at.eq(spec.post_at.map(clock::stamp)),
            schema::events::color.eq(&template.color),
            schema::events::image_url.eq(template.image_url.as_deref()),
            schema::events::cutoff_at.eq(cutoff.as_deref()),
            schema::events::mention_everyone.eq(template.mention_everyone),
            schema::events::attendee_role_id.eq(template.attendee_role_id),
            schema::events::thread_enabled.eq(template.thread_enabled),
            schema::events::multiple_signups.eq(template.multiple_signups),
            schema::events::created_by.eq(spec.created_by),
            schema::events::created_at.eq(&stamp),
            schema::events::updated_at.eq(&stamp),
        ))
        .returning(schema::events::id)
        .get_result(conn)?;
    replace_children(conn, id, &template.options, &template.reminders)?;
    for role_id in &template.mention_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("mention"),
            ))
            .execute(conn)?;
    }
    for role_id in &template.restrict_role_ids {
        diesel::insert_into(schema::event_roles::table)
            .values((
                schema::event_roles::event_id.eq(id),
                schema::event_roles::role_id.eq(role_id),
                schema::event_roles::kind.eq("restrict"),
            ))
            .execute(conn)?;
    }
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

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}
