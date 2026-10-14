//! Shared event wording. A series or a routine keeps one template.
//! Creating an event copies it and fills `{date}` with that event's date.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};

use super::db::must_guild;
use super::event::replace_children;
use super::model::{OptionInput, ReminderInput, SeriesTemplate};
use crate::clock;
use crate::error::OpError;
use crate::store::{self, Series};

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

pub fn insert(conn: &Connection, template: &EventTemplate) -> Result<i64, OpError> {
    conn.execute(
        "INSERT INTO event_templates (
            guild_id, channel_id, title, description, duration_minutes, color, image_url,
            thread_enabled, multiple_signups, mention_everyone, attendee_role_id,
            cutoff_offset_minutes, options_json, reminders_json, mention_role_ids,
            restrict_role_ids, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            template.guild_id,
            template.channel_id,
            template.title,
            template.description,
            template.duration_minutes,
            template.color,
            template.image_url,
            store::i(template.thread_enabled),
            store::i(template.multiple_signups),
            store::i(template.mention_everyone),
            template.attendee_role_id,
            template.cutoff_offset_minutes,
            to_json(&template.options),
            to_json(&template.reminders),
            to_json(&template.mention_role_ids),
            to_json(&template.restrict_role_ids),
            store::now_stamp(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update(conn: &Connection, id: i64, template: &EventTemplate) -> Result<(), OpError> {
    conn.execute(
        "UPDATE event_templates SET
            channel_id = ?, title = ?, description = ?, duration_minutes = ?, color = ?,
            image_url = ?, thread_enabled = ?, multiple_signups = ?, mention_everyone = ?,
            attendee_role_id = ?, cutoff_offset_minutes = ?, options_json = ?, reminders_json = ?,
            mention_role_ids = ?, restrict_role_ids = ?
         WHERE id = ?",
        params![
            template.channel_id,
            template.title,
            template.description,
            template.duration_minutes,
            template.color,
            template.image_url,
            store::i(template.thread_enabled),
            store::i(template.multiple_signups),
            store::i(template.mention_everyone),
            template.attendee_role_id,
            template.cutoff_offset_minutes,
            to_json(&template.options),
            to_json(&template.reminders),
            to_json(&template.mention_role_ids),
            to_json(&template.restrict_role_ids),
            id,
        ],
    )?;
    Ok(())
}

pub fn update_wording(
    conn: &Connection,
    id: i64,
    channel_id: i64,
    title: &str,
    description: &str,
    duration_minutes: i64,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE event_templates SET channel_id = ?, title = ?, description = ?, duration_minutes = ? WHERE id = ?",
        params![channel_id, title, description, duration_minutes, id],
    )?;
    Ok(())
}

pub fn load(conn: &Connection, id: i64) -> Result<EventTemplate, OpError> {
    conn.query_row(
        "SELECT id, guild_id, channel_id, title, description, duration_minutes, color, image_url,
                thread_enabled, multiple_signups, mention_everyone, attendee_role_id,
                cutoff_offset_minutes, options_json, reminders_json, mention_role_ids,
                restrict_role_ids
         FROM event_templates WHERE id = ?",
        [id],
        map_template,
    )
    .optional()?
    .ok_or(OpError::NotFound)
}

pub fn series_template(conn: &Connection, series: &Series) -> Result<SeriesTemplate, OpError> {
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
    conn: &Connection,
    template: &EventTemplate,
    spec: &Spawn<'_>,
) -> Result<i64, OpError> {
    let stamp = store::now_stamp();
    let cutoff = template
        .cutoff_offset_minutes
        .map(|mins| spec.starts_at - Duration::minutes(mins));
    conn.execute(
        "INSERT INTO events (
            public_id, guild_id, channel_id, series_id, occurrence_index, title, description, starts_at,
            duration_minutes, status, post_at, color, image_url, cutoff_at, mention_everyone,
            attendee_role_id, thread_enabled, multiple_signups, created_by, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            crate::ids::event_id(),
            template.guild_id,
            template.channel_id,
            spec.series_id,
            spec.occurrence_index,
            fill(&template.title, spec.starts_at, spec.tz),
            fill(&template.description, spec.starts_at, spec.tz),
            clock::stamp(spec.starts_at),
            template.duration_minutes,
            spec.status,
            spec.post_at.map(|dt| clock::stamp(dt)),
            template.color,
            template.image_url,
            cutoff.map(|dt| clock::stamp(dt)),
            store::i(template.mention_everyone),
            template.attendee_role_id,
            store::i(template.thread_enabled),
            store::i(template.multiple_signups),
            spec.created_by,
            stamp,
            stamp,
        ],
    )?;
    let id = conn.last_insert_rowid();
    replace_children(conn, id, &template.options, &template.reminders)?;
    for role_id in &template.mention_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'mention')",
            params![id, role_id],
        )?;
    }
    for role_id in &template.restrict_role_ids {
        conn.execute(
            "INSERT INTO event_roles (event_id, role_id, kind) VALUES (?, ?, 'restrict')",
            params![id, role_id],
        )?;
    }
    Ok(id)
}

pub fn fill_text(
    conn: &Connection,
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

fn map_template(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventTemplate> {
    let options: String = row.get(13)?;
    let reminders: String = row.get(14)?;
    let mentions: String = row.get(15)?;
    let restricts: String = row.get(16)?;
    Ok(EventTemplate {
        guild_id: row.get(1)?,
        channel_id: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        duration_minutes: row.get(5)?,
        color: row.get(6)?,
        image_url: row.get(7)?,
        thread_enabled: row.get::<_, i64>(8)? != 0,
        multiple_signups: row.get::<_, i64>(9)? != 0,
        mention_everyone: row.get::<_, i64>(10)? != 0,
        attendee_role_id: row.get(11)?,
        cutoff_offset_minutes: row.get(12)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        reminders: serde_json::from_str(&reminders).unwrap_or_default(),
        mention_role_ids: serde_json::from_str(&mentions).unwrap_or_default(),
        restrict_role_ids: serde_json::from_str(&restricts).unwrap_or_default(),
    })
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}
