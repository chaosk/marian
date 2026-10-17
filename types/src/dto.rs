//! REST resources. Times are UTC instants; the app words them in the server's zone.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::time::{ZoneGroup, ZoneRow};

/// The reply to a change: what to tell the person, and where to go next.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Done {
    pub notice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerRef {
    /// The Discord snowflake.
    pub id: String,
    pub name: String,
}

/// The signed-in person across servers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub name: String,
    pub servers: Vec<ServerRef>,
    pub last_server: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub drafts: i64,
    pub scheduled: i64,
    pub series: i64,
    pub routines: i64,
}

/// A server and the display choices every page words times with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub timezone: String,
    pub hour_24: bool,
    pub localize_time: bool,
    pub counts: Counts,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Google {
    pub ready: bool,
    pub connected: bool,
    pub reconnect: bool,
    pub auto_sync: bool,
    pub email: String,
}

/// The signed-in person in one server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Membership {
    pub id: i64,
    pub name: String,
    pub organizer: bool,
    pub reminders_enabled: bool,
    pub calendar_url: String,
    pub google: Google,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MembershipPatch {
    #[serde(default)]
    pub reminders_enabled: Option<bool>,
    #[serde(default)]
    pub google_auto_sync: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub events: i64,
    pub purged_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelDraft {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub timezone: String,
    pub default_reminder_minutes: i64,
    pub auto_archive_hours: i64,
    pub post_lead_hours: i64,
    pub digest: String,
    pub role_event: Option<i64>,
    pub role_edit: Option<i64>,
    pub role_delete: Option<i64>,
    pub role_channel: Option<i64>,
    pub role_signup: Option<i64>,
    pub thread_reminders: bool,
    pub auto_archive: bool,
    pub event_threads: bool,
    pub auto_join_threads: bool,
    pub show_attendees: bool,
    pub hour_24: bool,
    pub localize_time: bool,
    pub purge_channels: bool,
    pub temp_roles: bool,
    pub calendar_auto_sync: bool,
}

/// How full one option is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub emoji: String,
    pub name: String,
    pub capacity: Option<i64>,
    pub confirmed: i64,
    pub waiting: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub duration_minutes: i64,
    pub post_at: Option<DateTime<Utc>>,
    pub color: String,
    pub channel: String,
    pub spots: Vec<Spot>,
}

/// Which events a list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventView {
    /// Posted events that haven't ended, and queued posts.
    #[default]
    Upcoming,
    /// Posted events that have ended and wait for the archive.
    Wrapped,
    Drafts,
    /// Scheduled posts, including ones that failed to post.
    Queued,
    Archive,
    All,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventQuery {
    #[serde(default)]
    pub view: EventView,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub q: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub page: u32,
    pub pages: u32,
    pub per_page: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Abilities {
    pub edit: bool,
    pub delete: bool,
    pub archive: bool,
    pub skip: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesRef {
    pub id: String,
    pub cadence: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub description: String,
    pub location: String,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub duration_minutes: i64,
    pub cutoff_at: Option<DateTime<Utc>>,
    pub post_at: Option<DateTime<Utc>>,
    pub color: String,
    pub image_url: Option<String>,
    pub channel_id: i64,
    pub channel: String,
    pub attendee_role_id: Option<i64>,
    pub mention_everyone: bool,
    pub mention_role_ids: Vec<i64>,
    pub restrict_role_ids: Vec<i64>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub signups_closed: bool,
    pub series: Option<SeriesRef>,
    pub creator: String,
    pub discord_url: Option<String>,
    pub google_url: Option<String>,
    pub calendar_url: Option<String>,
    pub calendar_synced: bool,
    pub google_synced: bool,
    pub can: Abilities,
}

/// A signup option as stored, and as the event form sends it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignupOption {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    pub emoji: String,
    #[serde(default)]
    pub capacity: Option<i64>,
    #[serde(default)]
    pub waitlist: bool,
    #[serde(default)]
    pub role_id: Option<i64>,
    #[serde(default)]
    pub reminders: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub offset_minutes: i64,
    pub message: String,
    #[serde(default)]
    pub sent: bool,
}

/// A reminder still to go out, across the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingReminder {
    pub event_id: String,
    pub title: String,
    pub offset_minutes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub color: String,
}

/// One option's signups, and whether the signed-in person can use it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionSignups {
    pub id: i64,
    pub name: String,
    pub emoji: String,
    pub capacity: Option<i64>,
    pub role: Option<String>,
    pub reminders: bool,
    pub confirmed_count: i64,
    pub waiting_count: i64,
    /// Names, when the server shows them to this person.
    pub confirmed: Vec<Person>,
    pub waiting: Vec<Person>,
    pub mine: bool,
    pub open: bool,
    /// Why `open` is false.
    pub reason: String,
    /// The next signup lands on the waitlist.
    pub waitlist_next: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signups {
    pub options: Vec<OptionSignups>,
    pub notes: Vec<String>,
    pub show_names: bool,
    pub can_manage: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadNote {
    pub body: String,
    pub place: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub messages: Vec<ThreadNote>,
    pub members: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveAction {
    Draft,
    Schedule,
    #[default]
    Publish,
    Save,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Repeat {
    pub cadence: String,
    #[serde(default)]
    pub count: Option<i64>,
    #[serde(default)]
    pub until: Option<NaiveDate>,
}

/// Accepted, Declined, and Tentative, the options a new event starts with.
pub fn default_signup_options() -> Vec<SignupOption> {
    [
        ("Accepted", "✅", true),
        ("Declined", "❌", false),
        ("Tentative", "❔", false),
    ]
    .into_iter()
    .map(|(name, emoji, reminders)| SignupOption {
        id: None,
        name: name.into(),
        emoji: emoji.into(),
        capacity: None,
        waitlist: false,
        role_id: None,
        reminders,
    })
    .collect()
}

/// An event as the form sends it. Local times are in the server's zone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub channel_id: i64,
    pub color: String,
    pub starts_local: NaiveDateTime,
    pub duration_minutes: i64,
    #[serde(default)]
    pub cutoff_local: Option<NaiveDateTime>,
    #[serde(default)]
    pub post_local: Option<NaiveDateTime>,
    #[serde(default)]
    pub attendee_role_id: Option<i64>,
    #[serde(default)]
    pub thread_enabled: bool,
    #[serde(default)]
    pub multiple_signups: bool,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mention_role_ids: Vec<i64>,
    #[serde(default)]
    pub restrict_role_ids: Vec<i64>,
    #[serde(default)]
    pub options: Vec<SignupOption>,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
    #[serde(default)]
    pub repeat: Option<Repeat>,
    #[serde(default)]
    pub action: SaveAction,
    #[serde(default)]
    pub apply_series: bool,
}

/// An event form's starting values. Local times are datetime-local strings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventSeed {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub channel_id: i64,
    pub color: String,
    pub starts_local: String,
    pub duration_minutes: i64,
    #[serde(default)]
    pub cutoff_local: String,
    #[serde(default)]
    pub post_local: String,
    #[serde(default)]
    pub attendee_role_id: Option<i64>,
    #[serde(default)]
    pub thread_enabled: bool,
    #[serde(default)]
    pub multiple_signups: bool,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mention_role_ids: Vec<i64>,
    #[serde(default)]
    pub restrict_role_ids: Vec<i64>,
    #[serde(default)]
    pub options: Vec<SignupOption>,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
    #[serde(default)]
    pub repeat: Option<Repeat>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub title: String,
    pub cadence: String,
    pub active: bool,
    pub max_occurrences: Option<i64>,
    pub end_on: Option<DateTime<Utc>>,
    pub next: Option<DateTime<Utc>>,
    pub dates: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub multiple_signups: bool,
    pub options: Vec<SignupOption>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetDraft {
    pub name: String,
    #[serde(default)]
    pub multiple_signups: bool,
    /// Option names; known names take their emoji from the library.
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub name: String,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandLog {
    pub user: String,
    pub line: String,
    pub reply: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandRun {
    pub line: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandReply {
    pub reply: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayCheck {
    pub n: u8,
    pub name: String,
    pub on: bool,
    pub votes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutineView {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub description: String,
    pub active: bool,
    pub channel: String,
    pub channel_id: i64,
    pub poll_weekday: String,
    pub poll_time: String,
    pub lock_weekday: String,
    pub lock_time: String,
    pub start_time: String,
    pub poll_when: String,
    pub lock_when: String,
    pub event_when: String,
    pub duration_minutes: i64,
    pub usual: Vec<DayCheck>,
    pub run: Option<RunView>,
    pub history: Vec<RunHistory>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub id: i64,
    pub open: bool,
    pub status: String,
    pub days: Vec<DayCheck>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunHistory {
    pub when: String,
    pub winner: String,
    pub event_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutineSummary {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub active: bool,
    pub badge: String,
    pub poll: String,
    pub event: String,
    pub choices: Vec<PollChoice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PollChoice {
    pub label: String,
    pub votes: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineDraft {
    pub channel_id: i64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub duration_minutes: i64,
    /// "HH:MM" in the server's zone.
    pub start_time: String,
    pub poll_weekday: u8,
    pub poll_time: String,
    pub lock_weekday: u8,
    pub lock_time: String,
    pub days: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DayScope {
    /// The poll that is open now.
    #[default]
    Run,
    /// Polls for later weeks.
    Usual,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineDays {
    pub days: Vec<u8>,
    #[serde(default)]
    pub scope: DayScope,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vote {
    pub day: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Created {
    pub id: String,
    pub notice: String,
}
