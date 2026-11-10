//! REST resources. Times are UTC instants; the app words them in the server's zone.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::time::{ZoneGroup, ZoneRow};

/// What changed on a server, sent to its open pages so they fetch only that.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// Someone joined or left an event.
    Signups { event: String },
    /// An event's details, options, reminders, or status, or the event came or went.
    Event { event: String },
    /// Anything else: settings, presets, the library, series, routines, channels.
    Server,
}

/// The reply to a change: where to go next, when the change moved the person.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Done {
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
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub drafts: i64,
    pub upcoming: i64,
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
    pub name: String,
    pub server_manager: bool,
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
    pub id: String,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
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
    /// Roles that may create events. Empty lets everyone.
    #[serde(default)]
    pub role_event: Vec<String>,
    /// Roles that may edit any event, besides its creator and server managers.
    #[serde(default)]
    pub role_edit: Vec<String>,
    /// Roles that may delete any event, besides its creator and server managers.
    #[serde(default)]
    pub role_delete: Vec<String>,
    /// Roles that may create channels. Empty follows who may create events.
    #[serde(default)]
    pub role_channel: Vec<String>,
    /// Roles that may change signup options and presets.
    #[serde(default)]
    pub role_signup: Vec<String>,
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
    /// A full option takes more signups onto a waitlist that moves up as spots open.
    #[serde(default)]
    pub waitlist: bool,
    /// The language Marian writes in on Discord: "en" or "pl". Empty keeps it.
    #[serde(default)]
    pub language: String,
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

/// Whether an event has ended yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventWhen {
    /// Hasn't ended: coming up or happening now.
    Upcoming,
    /// Has ended. Listed newest first.
    Past,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventQuery {
    #[serde(default)]
    pub view: EventView,
    /// Only events with these statuses, comma-separated (`draft,scheduled`).
    /// Empty leaves it to `view`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<EventWhen>,
    /// The first and last day, in the server's zone, an event may start on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_day: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_day: Option<NaiveDate>,
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
    pub channel_id: String,
    pub channel: String,
    pub attendee_role_id: Option<String>,
    pub mention_everyone: bool,
    pub mention_role_ids: Vec<String>,
    pub restrict_role_ids: Vec<String>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub signups_closed: bool,
    /// The series this date belongs to, when it repeats.
    pub series: Option<Series>,
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
    /// The event's option, which keeps its responses. None for a new one.
    #[serde(default)]
    pub id: Option<String>,
    /// The attendance type from the server's library.
    pub type_id: String,
    /// The type's name, emoji, and attending, shown with it. The library sets
    /// them; a request that sends them is not heard.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub attending: bool,
    #[serde(default)]
    pub capacity: Option<i64>,
    #[serde(default)]
    pub role_id: Option<String>,
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
    pub id: String,
    pub name: String,
    pub color: String,
}

/// One option's signups, and whether the signed-in person can use it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionSignups {
    pub id: String,
    /// The attendance type from the server's library.
    pub type_id: String,
    pub name: String,
    pub emoji: String,
    pub capacity: Option<i64>,
    pub role_id: Option<String>,
    pub role: Option<String>,
    pub attending: bool,
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
    /// The options and responses can still change here: the person can manage
    /// the event, and it hasn't been archived, skipped, or cancelled.
    pub editable: bool,
    /// Responses can change only once the event is posted.
    pub posted: bool,
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

/// An event as the form sends it. Local times are in the server's zone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: String,
    pub channel_id: String,
    pub color: String,
    pub starts_local: NaiveDateTime,
    pub duration_minutes: i64,
    #[serde(default)]
    pub cutoff_local: Option<NaiveDateTime>,
    #[serde(default)]
    pub post_local: Option<NaiveDateTime>,
    #[serde(default)]
    pub attendee_role_id: Option<String>,
    #[serde(default)]
    pub thread_enabled: bool,
    #[serde(default)]
    pub multiple_signups: bool,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub mention_role_ids: Vec<String>,
    #[serde(default)]
    pub restrict_role_ids: Vec<String>,
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

/// When Marian posts a draft, set from the event page. The time is in the server's zone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PostAt {
    pub post_local: NaiveDateTime,
}

/// When signups close, set from the event page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Cutoff {
    Never,
    /// This many minutes before the event starts.
    Before { minutes: i64 },
}

/// One or more details changed in place on the event page. Unset fields stay as
/// they are. `starts_local` is in the server's zone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_local: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_signups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<Cutoff>,
    /// Empty takes the location away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Starts repeating an event that doesn't yet; a series changes on its own address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
    /// Only before the event has a Discord message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_enabled: Option<bool>,
    /// Copies this date's details to the upcoming dates of its series.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub apply_series: bool,
    /// The whole list; unset leaves the reminders as they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminders: Option<Vec<Reminder>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention_everyone: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention_role_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restrict_role_ids: Option<Vec<String>>,
    /// Unset leaves the role; `null` takes it away.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub attendee_role_id: Option<Option<String>>,
}

/// Reads a field that is there, even as `null`, as `Some`; a missing one stays `None`.
fn present<'de, D, T>(reader: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(reader).map(Some)
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
    /// The preset a new event starts with.
    #[serde(default)]
    pub default: bool,
    pub options: Vec<SignupOption>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetDraft {
    pub name: String,
    #[serde(default)]
    pub multiple_signups: bool,
    /// Types from the library, in order, with their capacity and role limit.
    pub options: Vec<SignupOption>,
}

/// Gives an event a preset's attendance.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyPreset {
    pub preset: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    pub name: String,
    pub emoji: String,
    /// People on this type count as attending: reminders and the attendee role.
    pub attending: bool,
    /// Where it is used, as "3 events and 1 preset"; empty when unused.
    #[serde(default)]
    pub used_by: String,
}

/// A library type as someone sets it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeDraft {
    pub name: String,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub attending: bool,
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
    /// On a poll, the date this day's event would fall on, as "Wed, Oct 21".
    #[serde(default)]
    pub date: String,
    /// On an open poll, who voted for this day. The Discord post lists them too.
    #[serde(default)]
    pub voters: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutineView {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub description: String,
    pub active: bool,
    pub channel: String,
    pub channel_id: String,
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
    pub event: RoutineEvent,
    pub run: Option<RunView>,
    pub history: Vec<RunHistory>,
    /// The event the last poll picked, while it is still ahead.
    #[serde(default)]
    pub upcoming: Option<RunHistory>,
    /// What happens each week, in order: the poll, any approval, the post.
    #[serde(default)]
    pub steps: Vec<RoutineStep>,
    /// A closed poll waiting for someone to confirm its day.
    #[serde(default)]
    pub approval: Option<ApprovalView>,
}

/// One step of a routine: "poll", "approval" or "post".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineStep {
    pub kind: String,
    /// Where an approval asks.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// Who an approval mentions and lets answer, besides server managers and
    /// the routine's creator.
    #[serde(default)]
    pub role_ids: Vec<String>,
}

/// A closed poll waiting for its day to be confirmed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalView {
    /// The channel the request is in.
    pub channel: String,
    /// The day that got the most votes.
    pub winner: u8,
    /// When the request lapses with no event.
    pub lapses: String,
    /// The poll's days with their votes, in date order. Days already begun are off.
    pub days: Vec<DayCheck>,
    /// Whether the person looking may answer.
    #[serde(default)]
    pub can_decide: bool,
}

/// An answer to an approval: post on `day`, or skip the week with none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub day: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub open: bool,
    pub status: String,
    /// Every weekday, in the order their events would fall after the poll locks.
    pub days: Vec<DayCheck>,
    #[serde(default)]
    pub opens: String,
    #[serde(default)]
    pub locks: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunHistory {
    pub when: String,
    pub winner: String,
    /// The event's page.
    pub href: String,
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
    /// The usual days and the time, as "Wed, Thu, Fri at 19:00".
    #[serde(default)]
    pub schedule: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PollChoice {
    pub label: String,
    pub votes: i64,
}

/// What each event a routine posts carries beyond its title, description,
/// channel, and time: the same details an event has.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineEvent {
    #[serde(default)]
    pub location: String,
    pub color: String,
    #[serde(default)]
    pub thread_enabled: bool,
    #[serde(default)]
    pub multiple_signups: bool,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub attendee_role_id: Option<String>,
    #[serde(default)]
    pub mention_role_ids: Vec<String>,
    #[serde(default)]
    pub restrict_role_ids: Vec<String>,
    /// Signups close this long before the event starts; none keeps them open.
    #[serde(default)]
    pub cutoff_minutes: Option<i64>,
    #[serde(default)]
    pub options: Vec<SignupOption>,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineDraft {
    pub channel_id: String,
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
    /// The rest of the event's details; unset keeps what the routine has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<RoutineEvent>,
    /// The routine's steps; unset keeps what the routine has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<RoutineStep>>,
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
