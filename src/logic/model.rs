use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::jobs::DiscordJob;
use crate::store::Event;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveAction {
    Draft,
    Schedule,
    Publish,
    Save,
}

#[derive(Debug, Clone)]
pub struct RepeatSpec {
    pub cadence: String,
    pub end_on: Option<DateTime<Utc>>,
    pub max_occurrences: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionInput {
    pub id: Option<i64>,
    pub name: String,
    pub emoji: String,
    pub capacity: Option<i64>,
    pub waitlist: bool,
    pub role_id: Option<i64>,
    pub reminders_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderInput {
    pub offset_minutes: i64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct EventInput {
    pub id: Option<i64>,
    pub guild_id: i64,
    pub actor_id: i64,
    pub channel_id: i64,
    pub title: String,
    pub description: String,
    pub location: String,
    pub starts_at: DateTime<Utc>,
    pub duration_minutes: i64,
    pub color: String,
    pub image_url: Option<String>,
    pub cutoff_at: Option<DateTime<Utc>>,
    pub attendee_role_id: Option<i64>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub mention_everyone: bool,
    pub mention_role_ids: Vec<i64>,
    pub restrict_role_ids: Vec<i64>,
    pub options: Vec<OptionInput>,
    pub reminders: Vec<ReminderInput>,
    pub repeat: Option<RepeatSpec>,
    pub action: SaveAction,
    pub post_at: Option<DateTime<Utc>>,
    pub apply_series: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesTemplate {
    pub title: String,
    pub description: String,
    pub duration_minutes: i64,
    pub channel_id: i64,
    pub color: String,
    pub image_url: Option<String>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub mention_everyone: bool,
    pub attendee_role_id: Option<i64>,
    pub mention_role_ids: Vec<i64>,
    pub restrict_role_ids: Vec<i64>,
    pub cutoff_offset_minutes: Option<i64>,
    pub options: Vec<OptionInput>,
    pub reminders: Vec<ReminderInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetBody {
    pub multiple_signups: bool,
    pub options: Vec<OptionInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonRole {
    Signup(i64),
    Edit,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonTone {
    Secondary,
    Primary,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordButton {
    pub role: ButtonRole,
    pub label: String,
    pub emoji: String,
    pub tone: ButtonTone,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Debug, Clone)]
pub struct DiscordView {
    pub channel_id: String,
    pub message_id: Option<String>,
    pub title: String,
    pub body: String,
    pub color: u32,
    pub fields: Vec<DiscordField>,
    pub footer: String,
    pub image_url: Option<String>,
    pub buttons: Vec<DiscordButton>,
    pub mention_everyone: bool,
    /// Discord role ids to mention above the embed.
    pub mention_roles: Vec<String>,
}

pub struct TickReport {
    pub notes: Vec<String>,
    pub jobs: Vec<DiscordJob>,
}

pub struct CleanInput {
    pub title: String,
    pub description: String,
    pub location: String,
    pub image_url: Option<String>,
    pub options: Vec<OptionInput>,
    pub reminders: Vec<ReminderInput>,
    pub existing: Option<Event>,
}
