use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use diesel::connection::Connection as DbConnection;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::OpError;
use crate::schema;

pub use diesel::pg::PgConnection as Connection;

pub type Db = diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<Connection>>;

static TEST_SCHEMAS: AtomicU64 = AtomicU64::new(0);
static TEST_CLEANUP: Once = Once::new();

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Debug, Clone, HasQuery, Identifiable, AsChangeset)]
#[diesel(table_name = crate::schema::guilds)]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Guild {
    pub id: i64,
    pub name: String,
    pub timezone: String,
    pub discord_id: Option<String>,
}

/// A server's settings row, without the roles each permission names.
#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::settings)]
#[diesel(primary_key(guild_id))]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SettingsRow {
    guild_id: i64,
    default_reminder_minutes: i64,
    thread_reminders: bool,
    auto_archive: bool,
    auto_archive_hours: i64,
    event_threads: bool,
    auto_join_threads: bool,
    show_attendees: bool,
    hour_24: bool,
    localize_time: bool,
    purge_channels: bool,
    temp_roles: bool,
    calendar_auto_sync: bool,
    post_lead_hours: i64,
    digest: String,
    waitlist: bool,
    default_preset_id: Option<i64>,
    language: String,
}

/// A server's settings. Each `role_*` list names the roles that may do one
/// thing; an empty list leaves it to the default for that permission.
#[derive(Debug, Clone)]
pub struct Settings {
    pub guild_id: i64,
    pub default_reminder_minutes: i64,
    pub thread_reminders: bool,
    pub auto_archive: bool,
    pub auto_archive_hours: i64,
    pub event_threads: bool,
    pub auto_join_threads: bool,
    pub show_attendees: bool,
    pub hour_24: bool,
    pub localize_time: bool,
    pub purge_channels: bool,
    pub temp_roles: bool,
    pub calendar_auto_sync: bool,
    pub role_event: Vec<i64>,
    pub role_edit: Vec<i64>,
    pub role_delete: Vec<i64>,
    pub role_channel: Vec<i64>,
    pub role_signup: Vec<i64>,
    pub post_lead_hours: i64,
    pub digest: String,
    /// A full option takes more signups onto a waitlist that moves up as spots open.
    pub waitlist: bool,
    /// The preset a new event starts with.
    pub default_preset_id: Option<i64>,
    /// The language Marian writes in on Discord: "en" or "pl".
    pub language: String,
}

/// The permission names stored with each role, and the matching settings field.
const PERMISSION_KINDS: [&str; 5] = ["event", "edit", "delete", "channel", "signup"];

impl Settings {
    fn permission(&self, kind: &str) -> &[i64] {
        match kind {
            "event" => &self.role_event,
            "edit" => &self.role_edit,
            "delete" => &self.role_delete,
            "channel" => &self.role_channel,
            _ => &self.role_signup,
        }
    }

    fn permission_mut(&mut self, kind: &str) -> &mut Vec<i64> {
        match kind {
            "event" => &mut self.role_event,
            "edit" => &mut self.role_edit,
            "delete" => &mut self.role_delete,
            "channel" => &mut self.role_channel,
            _ => &mut self.role_signup,
        }
    }

    fn row(&self) -> SettingsRow {
        SettingsRow {
            guild_id: self.guild_id,
            default_reminder_minutes: self.default_reminder_minutes,
            thread_reminders: self.thread_reminders,
            auto_archive: self.auto_archive,
            auto_archive_hours: self.auto_archive_hours,
            event_threads: self.event_threads,
            auto_join_threads: self.auto_join_threads,
            show_attendees: self.show_attendees,
            hour_24: self.hour_24,
            localize_time: self.localize_time,
            purge_channels: self.purge_channels,
            temp_roles: self.temp_roles,
            calendar_auto_sync: self.calendar_auto_sync,
            post_lead_hours: self.post_lead_hours,
            digest: self.digest.clone(),
            waitlist: self.waitlist,
            default_preset_id: self.default_preset_id,
            language: self.language.clone(),
        }
    }
}

impl From<SettingsRow> for Settings {
    fn from(row: SettingsRow) -> Self {
        Self {
            guild_id: row.guild_id,
            default_reminder_minutes: row.default_reminder_minutes,
            thread_reminders: row.thread_reminders,
            auto_archive: row.auto_archive,
            auto_archive_hours: row.auto_archive_hours,
            event_threads: row.event_threads,
            auto_join_threads: row.auto_join_threads,
            show_attendees: row.show_attendees,
            hour_24: row.hour_24,
            localize_time: row.localize_time,
            purge_channels: row.purge_channels,
            temp_roles: row.temp_roles,
            calendar_auto_sync: row.calendar_auto_sync,
            role_event: Vec::new(),
            role_edit: Vec::new(),
            role_delete: Vec::new(),
            role_channel: Vec::new(),
            role_signup: Vec::new(),
            post_lead_hours: row.post_lead_hours,
            digest: row.digest,
            waitlist: row.waitlist,
            default_preset_id: row.default_preset_id,
            language: row.language,
        }
    }
}

#[derive(Debug, Clone, HasQuery, Identifiable)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: i64,
    pub discord_id: String,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::memberships)]
#[diesel(belongs_to(User))]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Membership {
    pub id: i64,
    pub user_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub handle: String,
    pub color: String,
    pub reminders_enabled: bool,
    pub server_manager: bool,
    /// The id the API shows. The sequential `id` never leaves the server.
    pub public_id: uuid::Uuid,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::roles)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Role {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub color: String,
    pub discord_id: Option<String>,
    /// The id the API shows. The sequential `id` never leaves the server.
    pub public_id: uuid::Uuid,
    /// When the Discord role was deleted; the row stays for what names it.
    pub gone_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::channels)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Channel {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub kind: String,
    pub position: i64,
    pub purged_at: Option<DateTime<Utc>>,
    pub discord_id: Option<String>,
    /// The id the API shows. The sequential `id` never leaves the server.
    pub public_id: uuid::Uuid,
    /// When the Discord channel was deleted; the row stays for its events.
    pub gone_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::events)]
#[diesel(belongs_to(Guild))]
#[diesel(belongs_to(Channel))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Event {
    pub id: i64,
    pub public_id: Uuid,
    pub guild_id: i64,
    pub channel_id: i64,
    pub series_id: Option<i64>,
    pub occurrence_index: Option<i64>,
    pub title: String,
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub duration_minutes: i64,
    pub status: String,
    pub post_at: Option<DateTime<Utc>>,
    pub posted_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub color: String,
    pub image_url: Option<String>,
    pub cutoff_at: Option<DateTime<Utc>>,
    pub mention_everyone: bool,
    pub attendee_role_id: Option<i64>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub calendar_synced_at: Option<DateTime<Utc>>,
    pub signups_closed: bool,
    pub post_attempts: i64,
    pub discord_message_id: Option<String>,
    pub location: String,
}

#[derive(Debug, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::event_templates)]
#[diesel(belongs_to(Guild))]
#[diesel(belongs_to(Channel))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub(crate) struct StoredTemplate {
    #[allow(dead_code)]
    pub(crate) id: i64,
    pub(crate) guild_id: i64,
    pub(crate) channel_id: i64,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) duration_minutes: i64,
    pub(crate) color: String,
    pub(crate) image_url: Option<String>,
    pub(crate) thread_enabled: bool,
    pub(crate) multiple_signups: bool,
    pub(crate) mention_everyone: bool,
    pub(crate) attendee_role_id: Option<i64>,
    pub(crate) cutoff_offset_minutes: Option<i64>,
    pub(crate) reminders_json: serde_json::Value,
    pub(crate) mention_role_ids: Vec<i64>,
    pub(crate) restrict_role_ids: Vec<i64>,
    #[allow(dead_code)]
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) location: String,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::series)]
#[diesel(belongs_to(Guild))]
#[diesel(belongs_to(StoredTemplate, foreign_key = template_id))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Series {
    pub id: i64,
    pub public_id: Uuid,
    pub guild_id: i64,
    pub cadence: String,
    pub end_on: Option<DateTime<Utc>>,
    pub max_occurrences: Option<i64>,
    pub active: bool,
    pub template_id: i64,
    pub anchor_start: DateTime<Utc>,
    pub post_lead_hours: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::routines)]
#[diesel(belongs_to(Guild))]
#[diesel(belongs_to(StoredTemplate, foreign_key = template_id))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Routine {
    pub id: i64,
    pub public_id: Uuid,
    pub guild_id: i64,
    pub template_id: i64,
    pub prompt: String,
    pub start_minutes: i64,
    pub poll_weekday: i64,
    pub poll_minutes: i64,
    pub lock_weekday: i64,
    pub lock_minutes: i64,
    pub days_json: Vec<i16>,
    pub active: bool,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub description: String,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::routine_runs)]
#[diesel(belongs_to(Routine))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RoutineRun {
    pub id: i64,
    pub routine_id: i64,
    pub opens_at: DateTime<Utc>,
    pub locks_at: DateTime<Utc>,
    pub status: String,
    pub days_json: Vec<i16>,
    pub winner_weekday: Option<i64>,
    pub event_id: Option<i64>,
    pub discord_message_id: Option<String>,
    pub created_at: DateTime<Utc>,
    /// The id its Discord buttons carry. The sequential `id` never leaves the server.
    pub public_id: uuid::Uuid,
    /// The reply with the results, once the poll has locked.
    pub results_message_id: Option<String>,
    /// The position of the routine step this run is on.
    pub step: i64,
    /// The request an approval step posted.
    pub approval_message_id: Option<String>,
    /// When an approval that nobody answered lapses.
    pub decide_by: Option<DateTime<Utc>>,
    /// Who answered the approval.
    pub decided_by: Option<i64>,
}

/// An event's signup option as stored: which library type it is, and what this
/// event sets for it.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::signup_options)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SignupOptionRow {
    id: i64,
    event_id: i64,
    capacity: Option<i64>,
    role_id: Option<i64>,
    position: i64,
    public_id: uuid::Uuid,
    type_id: i64,
    max_guests: i64,
}

/// An event's signup option, with its type's name, emoji, and attending read
/// from the server's library. Renaming the type renames it here too.
#[derive(Debug, Clone)]
pub struct SignupOption {
    pub id: i64,
    pub event_id: i64,
    /// The library type.
    pub type_id: i64,
    pub type_public_id: uuid::Uuid,
    pub name: String,
    pub emoji: String,
    pub capacity: Option<i64>,
    /// Guests one signup may bring; 0 lets none.
    pub max_guests: i64,
    pub role_id: Option<i64>,
    /// Whether the type counts as attending: reminders and the attendee role.
    pub reminders_enabled: bool,
    pub position: i64,
    /// The id the API shows. The sequential `id` never leaves the server.
    pub public_id: uuid::Uuid,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::signups)]
#[diesel(belongs_to(Event))]
#[diesel(belongs_to(SignupOption, foreign_key = option_id))]
#[diesel(belongs_to(Membership))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Signup {
    pub id: i64,
    pub event_id: i64,
    pub option_id: i64,
    pub membership_id: i64,
    pub state: String,
    pub created_at: DateTime<Utc>,
    /// Guests this person brings. The signup takes 1 + guests places.
    pub guests: i64,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::reminders)]
#[diesel(belongs_to(Event))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Reminder {
    pub id: i64,
    pub event_id: i64,
    pub offset_minutes: i64,
    pub message: String,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::activity)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Activity {
    pub id: i64,
    pub guild_id: i64,
    pub event_id: Option<i64>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::presets)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Preset {
    pub id: i64,
    pub public_id: Uuid,
    pub guild_id: i64,
    pub name: String,
    pub multiple_signups: bool,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::option_library)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct LibraryOption {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub emoji: String,
    /// The id the API shows. The sequential `id` never leaves the server.
    pub public_id: Uuid,
    /// People on this type count as attending: reminders and the attendee role.
    pub attending: bool,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::thread_messages)]
#[diesel(belongs_to(Event))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ThreadMessage {
    pub id: i64,
    pub event_id: i64,
    pub body: String,
    pub place: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::command_log)]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CommandRow {
    pub id: i64,
    pub guild_id: i64,
    pub membership_id: Option<i64>,
    pub line: String,
    pub reply: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::membership_roles)]
#[diesel(primary_key(membership_id, role_id))]
#[diesel(belongs_to(Membership))]
#[diesel(belongs_to(Role))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MembershipRole {
    pub membership_id: i64,
    pub role_id: i64,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::event_roles)]
#[diesel(primary_key(event_id, role_id, kind))]
#[diesel(belongs_to(Event))]
#[diesel(belongs_to(Role))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct EventRole {
    pub event_id: i64,
    pub role_id: i64,
    pub kind: String,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::thread_members)]
#[diesel(primary_key(event_id, membership_id))]
#[diesel(belongs_to(Event))]
#[diesel(belongs_to(Membership))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ThreadMember {
    pub event_id: i64,
    pub membership_id: i64,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::role_grants)]
#[diesel(primary_key(event_id, membership_id))]
#[diesel(belongs_to(Event))]
#[diesel(belongs_to(Membership))]
#[diesel(belongs_to(Role))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RoleGrant {
    pub event_id: i64,
    pub membership_id: i64,
    pub role_id: i64,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::web_sessions)]
#[diesel(primary_key(token))]
#[diesel(belongs_to(Membership))]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SessionRow {
    token: String,
    discord_user_id: String,
    display_name: String,
    guild_id: i64,
    membership_id: i64,
    created_at: DateTime<Utc>,
    discord_access: Option<String>,
    discord_refresh: Option<String>,
    discord_expires_at: Option<DateTime<Utc>>,
    guilds_checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations)]
#[diesel(table_name = crate::schema::web_session_guilds)]
#[diesel(primary_key(token, guild_id))]
#[diesel(belongs_to(SessionRow, foreign_key = token))]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct SessionGuild {
    token: String,
    guild_id: i64,
}

macro_rules! model_query {
    ($alias:ident, $model:ty) => {
        type $alias = diesel::dsl::Select<
            <$model as diesel::prelude::HasQuery<diesel::pg::Pg>>::BaseQuery,
            diesel::dsl::AsSelect<$model, diesel::pg::Pg>,
        >;
    };
}

model_query!(GuildQuery, Guild);
model_query!(RoleQuery, Role);
model_query!(ChannelQuery, Channel);
model_query!(MembershipQuery, Membership);
model_query!(EventQuery, Event);
model_query!(SeriesQuery, Series);
model_query!(RoutineRunQuery, RoutineRun);
model_query!(SignupQuery, Signup);
model_query!(ReminderQuery, Reminder);
model_query!(ActivityQuery, Activity);
model_query!(PresetQuery, Preset);

impl Guild {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let guilds: GuildQuery = Guild::query();
        guilds.filter(schema::guilds::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_discord(discord_id: &str) -> _ {
        let guilds: GuildQuery = Guild::query();
        guilds.filter(schema::guilds::discord_id.eq(discord_id))
    }
}

impl Role {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let roles: RoleQuery = Role::query();
        roles.filter(schema::roles::guild_id.eq(guild_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let roles: RoleQuery = Role::query();
        roles.filter(schema::roles::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn named(guild_id: i64, name: &str) -> _ {
        let roles: RoleQuery = Role::query();
        roles
            .filter(schema::roles::guild_id.eq(guild_id))
            .filter(schema::roles::gone_at.is_null())
            .filter(schema::lower(schema::roles::name).eq(schema::lower(name)))
    }
}

impl Channel {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let channels: ChannelQuery = Channel::query();
        channels.filter(schema::channels::guild_id.eq(guild_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let channels: ChannelQuery = Channel::query();
        channels.filter(schema::channels::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn named(guild_id: i64, name: &str) -> _ {
        let channels: ChannelQuery = Channel::query();
        channels
            .filter(schema::channels::guild_id.eq(guild_id))
            .filter(schema::channels::gone_at.is_null())
            .filter(schema::lower(schema::channels::name).eq(schema::lower(name)))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_discord(guild_id: i64, discord_id: &str) -> _ {
        let channels: ChannelQuery = Channel::query();
        channels
            .filter(schema::channels::guild_id.eq(guild_id))
            .filter(schema::channels::discord_id.eq(discord_id))
    }
}

impl Membership {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let memberships: MembershipQuery = Membership::query();
        memberships.filter(schema::memberships::guild_id.eq(guild_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let memberships: MembershipQuery = Membership::query();
        memberships.filter(schema::memberships::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_handle(guild_id: i64, handle: &str) -> _ {
        let memberships: MembershipQuery = Membership::query();
        memberships
            .filter(schema::memberships::guild_id.eq(guild_id))
            .filter(schema::lower(schema::memberships::handle).eq(schema::lower(handle)))
    }
}

impl Event {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let events: EventQuery = Event::query();
        events.filter(schema::events::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_public(public_id: Uuid) -> _ {
        let events: EventQuery = Event::query();
        events.filter(schema::events::public_id.eq(public_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let events: EventQuery = Event::query();
        events.filter(schema::events::guild_id.eq(guild_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_series(series_id: i64) -> _ {
        let events: EventQuery = Event::query();
        events.filter(schema::events::series_id.eq(series_id))
    }
}

impl Series {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let series: SeriesQuery = Series::query();
        series.filter(schema::series::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_public(public_id: Uuid) -> _ {
        let series: SeriesQuery = Series::query();
        series.filter(schema::series::public_id.eq(public_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let series: SeriesQuery = Series::query();
        series.filter(schema::series::guild_id.eq(guild_id))
    }
}

impl RoutineRun {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let runs: RoutineRunQuery = RoutineRun::query();
        runs.filter(schema::routine_runs::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_routine(routine_id: i64) -> _ {
        let runs: RoutineRunQuery = RoutineRun::query();
        runs.filter(schema::routine_runs::routine_id.eq(routine_id))
    }
}

impl Signup {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_event(event_id: i64) -> _ {
        let signups: SignupQuery = Signup::query();
        signups
            .filter(schema::signups::event_id.eq(event_id))
            .order((schema::signups::created_at.asc(), schema::signups::id.asc()))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_events(event_ids: &[i64]) -> _ {
        let signups: SignupQuery = Signup::query();
        signups
            .filter(schema::signups::event_id.eq_any(event_ids))
            .order((schema::signups::created_at.asc(), schema::signups::id.asc()))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_option_member(option_id: i64, membership_id: i64) -> _ {
        let signups: SignupQuery = Signup::query();
        signups
            .filter(schema::signups::option_id.eq(option_id))
            .filter(schema::signups::membership_id.eq(membership_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_event_member(event_id: i64, membership_id: i64) -> _ {
        let signups: SignupQuery = Signup::query();
        signups
            .filter(schema::signups::event_id.eq(event_id))
            .filter(schema::signups::membership_id.eq(membership_id))
    }
}

impl Reminder {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_event(event_id: i64) -> _ {
        let reminders: ReminderQuery = Reminder::query();
        reminders
            .filter(schema::reminders::event_id.eq(event_id))
            .order((
                schema::reminders::offset_minutes.desc(),
                schema::reminders::id.asc(),
            ))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_events(event_ids: &[i64]) -> _ {
        let reminders: ReminderQuery = Reminder::query();
        reminders
            .filter(schema::reminders::event_id.eq_any(event_ids))
            .order((
                schema::reminders::offset_minutes.desc(),
                schema::reminders::id.asc(),
            ))
    }
}

impl Activity {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let activity: ActivityQuery = Activity::query();
        activity.filter(schema::activity::guild_id.eq(guild_id))
    }
}

impl Preset {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn in_guild(guild_id: i64) -> _ {
        let presets: PresetQuery = Preset::query();
        presets.filter(schema::presets::guild_id.eq(guild_id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let presets: PresetQuery = Preset::query();
        presets.filter(schema::presets::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_public(public_id: Uuid) -> _ {
        let presets: PresetQuery = Preset::query();
        presets.filter(schema::presets::public_id.eq(public_id))
    }
}

pub fn open(database_url: &str) -> Result<Connection, OpError> {
    let url = parse_database_url(database_url)?;
    let mut conn = connect(&url)?;
    crate::observe::install_query_trace(&mut conn, "postgresql");
    migrate(&mut conn)?;
    Ok(conn)
}

pub fn pool(database_url: &str) -> Result<Db, OpError> {
    let url = parse_database_url(database_url)?;
    build_pool(&url, None, pool_size())
}

pub fn pool_for(conn: &mut Connection) -> Result<Db, OpError> {
    let schema: String =
        diesel::select(diesel::dsl::sql::<diesel::sql_types::Text>("current_schema()"))
            .get_result(conn)?;
    build_pool(&database_url()?, Some(schema), 4)
}

fn pool_size() -> u32 {
    std::env::var("MARIAN_DB_POOL")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(10)
}

fn build_pool(url: &str, schema: Option<String>, max_size: u32) -> Result<Db, OpError> {
    let mut conn = connect(url)?;
    if let Some(schema) = &schema {
        set_search_path(&mut conn, schema)?;
    }
    crate::observe::install_query_trace(&mut conn, "postgresql");
    migrate(&mut conn)?;
    drop(conn);
    let manager = diesel::r2d2::ConnectionManager::<Connection>::new(url);
    diesel::r2d2::Pool::builder()
        .max_size(max_size)
        .test_on_check_out(true)
        .connection_customizer(Box::new(Prepare { schema }))
        .build(manager)
        .map_err(|err| OpError::message(format!("Could not open the database: {err}")))
}

#[derive(Debug)]
struct Prepare {
    schema: Option<String>,
}

impl diesel::r2d2::CustomizeConnection<Connection, diesel::r2d2::Error> for Prepare {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), diesel::r2d2::Error> {
        if let Some(schema) = &self.schema {
            set_search_path(conn, schema).map_err(diesel::r2d2::Error::QueryError)?;
        }
        crate::observe::install_query_trace(conn, "postgresql");
        Ok(())
    }
}

fn set_search_path(conn: &mut Connection, schema: &str) -> Result<(), diesel::result::Error> {
    if !schema
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(diesel::result::Error::QueryBuilderError(
            "The database schema name is not usable.".into(),
        ));
    }
    conn.batch_execute(&format!("SET search_path TO {schema}"))
}

struct SchemaGuard {
    schema: String,
    admin_url: String,
}

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        drop_schema(&self.admin_url, &self.schema);
    }
}

thread_local! {
    static TEST_GUARDS: RefCell<Vec<SchemaGuard>> = const { RefCell::new(Vec::new()) };
}

pub fn open_memory() -> Result<Connection, OpError> {
    let admin_url = database_url()?;
    let schema = format!(
        "marian_t{}_{}",
        std::process::id(),
        TEST_SCHEMAS.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = connect(&admin_url)?;
    let pid = std::process::id();
    TEST_CLEANUP.call_once(|| {
        let _ = admin.batch_execute(&format!(
            "DO $$ DECLARE name text; BEGIN
               FOR name IN SELECT nspname FROM pg_namespace WHERE nspname LIKE 'marian_t{pid}_%'
               LOOP EXECUTE format('DROP SCHEMA IF EXISTS %I CASCADE', name); END LOOP;
             END $$"
        ));
    });
    admin
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .map_err(|err| OpError::message(format!("Could not open the database: {err}")))?;
    drop(admin);
    let mut conn = match connect(&admin_url) {
        Ok(conn) => conn,
        Err(err) => {
            drop_schema(&admin_url, &schema);
            return Err(err);
        }
    };
    if let Err(err) = conn.batch_execute(&format!("SET search_path TO {schema}")) {
        drop(conn);
        drop_schema(&admin_url, &schema);
        return Err(OpError::message(format!("Could not open the database: {err}")));
    }
    crate::observe::install_query_trace(&mut conn, "postgresql");
    if let Err(err) = migrate(&mut conn) {
        drop(conn);
        drop_schema(&admin_url, &schema);
        return Err(err);
    }
    let guard = SchemaGuard {
        schema,
        admin_url,
    };
    TEST_GUARDS.with(|guards| guards.borrow_mut().push(guard));
    Ok(conn)
}

fn connect(url: &str) -> Result<Connection, OpError> {
    Connection::establish(url)
        .map_err(|err| OpError::message(format!("Could not open the database: {err}")))
}

fn drop_schema(admin_url: &str, schema: &str) {
    if admin_url.is_empty() || schema.is_empty() {
        return;
    }
    let Ok(mut admin) = Connection::establish(admin_url) else {
        return;
    };
    let _ = admin.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"));
}

fn parse_database_url(database_url: &str) -> Result<String, OpError> {
    let database_url = database_url.trim();
    if database_url.is_empty() {
        return Err(OpError::message(
            "Set DATABASE_URL to a Postgres connection string, such as postgres://marian:marian@127.0.0.1:5432/marian.",
        ));
    }
    let database_url = normalize_scheme(database_url);
    let Some((scheme, _)) = database_url.split_once("://") else {
        return Err(OpError::message(
            "Set DATABASE_URL to a Postgres connection string, such as postgres://marian:marian@127.0.0.1:5432/marian.",
        ));
    };
    if scheme != "postgres" && scheme != "postgresql" {
        return Err(OpError::message(format!(
            "Set DATABASE_URL to a Postgres connection string, such as postgres://marian:marian@127.0.0.1:5432/marian. The scheme is {scheme}."
        )));
    }
    Ok(database_url)
}

fn normalize_scheme(database_url: &str) -> String {
    let Some((scheme, rest)) = database_url.split_once(':') else {
        return database_url.to_string();
    };
    if scheme.is_empty() || !scheme.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return database_url.to_string();
    }
    format!("{}:{rest}", scheme.to_ascii_lowercase())
}

pub fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        let Some((key, value)) = parse_dotenv_line(line) else {
            continue;
        };
        if env_value(&key).is_some() {
            continue;
        }
        // SAFETY: startup runs this before any other thread reads the environment.
        unsafe { std::env::set_var(key, value) };
    }
}

pub fn database_url() -> Result<String, OpError> {
    let raw = env_value("DATABASE_URL")
        .or_else(|| dotenv_value("DATABASE_URL"))
        .ok_or_else(|| {
            OpError::message(
                "Set DATABASE_URL to a Postgres connection string, such as postgres://marian:marian@127.0.0.1:5432/marian.",
            )
        })?;
    parse_database_url(&raw)
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn dotenv_value(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(".env").ok()?;
    text.lines().find_map(|line| {
        let (name, value) = parse_dotenv_line(line)?;
        (name == key).then_some(value)
    })
}

fn parse_dotenv_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = value.trim().trim_matches(|ch| ch == '"' || ch == '\'');
    if value.is_empty() {
        return None;
    }
    Some((name.to_string(), value.to_string()))
}

pub fn migrate(conn: &mut Connection) -> Result<(), OpError> {
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|err| OpError::message(format!("Could not update the database: {err}")))?;
    Ok(())
}

pub fn with_tx<T>(
    conn: &mut Connection,
    body: impl FnOnce(&mut Connection) -> Result<T, OpError>,
) -> Result<T, OpError> {
    conn.transaction(body)
}

pub fn meta(conn: &mut Connection, key: &str) -> Result<Option<String>, OpError> {
    Ok(schema::meta::table
        .filter(schema::meta::key.eq(key))
        .select(schema::meta::value)
        .first(conn)
        .optional()?)
}

#[derive(Insertable)]
#[diesel(table_name = schema::meta)]
struct NewMeta<'a> {
    key: &'a str,
    value: &'a str,
}

pub fn set_meta(conn: &mut Connection, key: &str, value: &str) -> Result<(), OpError> {
    diesel::insert_into(schema::meta::table)
        .values(&NewMeta { key, value })
        .on_conflict(schema::meta::key)
        .do_update()
        .set(schema::meta::value.eq(value))
        .execute(conn)?;
    Ok(())
}

pub fn list_guilds(conn: &mut Connection) -> Result<Vec<Guild>, OpError> {
    Ok(Guild::query()
        .order(schema::guilds::id.asc())
        .load(conn)?)
}

pub fn guild(conn: &mut Connection, id: i64) -> Result<Option<Guild>, OpError> {
    Ok(Guild::by_id(id).first(conn).optional()?)
}

pub fn guild_by_discord(conn: &mut Connection, discord_id: &str) -> Result<Option<Guild>, OpError> {
    Ok(Guild::by_discord(discord_id).first(conn).optional()?)
}

pub fn default_settings(guild_id: i64) -> Settings {
    Settings {
        guild_id,
        default_reminder_minutes: 15,
        thread_reminders: true,
        auto_archive: true,
        auto_archive_hours: 12,
        event_threads: true,
        auto_join_threads: false,
        show_attendees: true,
        hour_24: false,
        localize_time: true,
        purge_channels: true,
        temp_roles: false,
        calendar_auto_sync: true,
        role_event: Vec::new(),
        role_edit: Vec::new(),
        role_delete: Vec::new(),
        role_channel: Vec::new(),
        role_signup: Vec::new(),
        post_lead_hours: 48,
        digest: "off".into(),
        waitlist: false,
        default_preset_id: None,
        language: "en".into(),
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::settings)]
struct NewSettings {
    guild_id: i64,
}

/// A new server's settings, and the attendance it starts with: Accepted,
/// Declined, and Tentative in the library, and a Standard preset listing them
/// as the default.
pub fn insert_default_settings(conn: &mut Connection, guild_id: i64) -> Result<(), OpError> {
    diesel::insert_into(schema::settings::table)
        .values(&NewSettings { guild_id })
        .execute(conn)?;
    seed_attendance(conn, guild_id)
}

/// The types every server starts with: name, emoji, and whether they count as attending.
pub const STARTING_TYPES: [(&str, &str, bool); 3] = [
    ("Accepted", "✅", true),
    ("Declined", "❌", false),
    ("Tentative", "❔", false),
];

/// Puts the starting types in the library and, when the server has no default
/// preset, makes a Standard one with them. What is already there stays.
pub fn seed_attendance(conn: &mut Connection, guild_id: i64) -> Result<(), OpError> {
    let mut types = Vec::new();
    for (name, emoji, attending) in STARTING_TYPES {
        types.push(insert_type(conn, guild_id, name, emoji, attending)?);
    }
    if settings(conn, guild_id)?.default_preset_id.is_some() {
        return Ok(());
    }
    let preset_id: i64 = diesel::insert_into(schema::presets::table)
        .values((
            schema::presets::public_id.eq(crate::ids::preset_id()),
            schema::presets::guild_id.eq(guild_id),
            schema::presets::name.eq("Standard"),
            schema::presets::multiple_signups.eq(false),
        ))
        .returning(schema::presets::id)
        .get_result(conn)?;
    let rows: Vec<_> = types
        .iter()
        .enumerate()
        .map(|(position, type_id)| {
            (
                schema::preset_options::preset_id.eq(preset_id),
                schema::preset_options::type_id.eq(*type_id),
                schema::preset_options::position.eq(position as i64),
            )
        })
        .collect();
    diesel::insert_into(schema::preset_options::table).values(&rows).execute(conn)?;
    set_default_preset(conn, guild_id, Some(preset_id))
}

/// The library type with this name (any case), made if missing.
pub fn insert_type(conn: &mut Connection, guild_id: i64, name: &str, emoji: &str, attending: bool) -> Result<i64, OpError> {
    if let Some(found) = library_type_named(conn, guild_id, name)? {
        return Ok(found.id);
    }
    Ok(diesel::insert_into(schema::option_library::table)
        .values((
            schema::option_library::guild_id.eq(guild_id),
            schema::option_library::name.eq(name),
            schema::option_library::emoji.eq(emoji),
            schema::option_library::attending.eq(attending),
        ))
        .returning(schema::option_library::id)
        .get_result(conn)?)
}

pub fn set_default_preset(conn: &mut Connection, guild_id: i64, preset_id: Option<i64>) -> Result<(), OpError> {
    diesel::update(schema::settings::table.filter(schema::settings::guild_id.eq(guild_id)))
        .set(schema::settings::default_preset_id.eq(preset_id))
        .execute(conn)?;
    Ok(())
}

pub fn library_type(conn: &mut Connection, id: i64) -> Result<Option<LibraryOption>, OpError> {
    Ok(LibraryOption::query()
        .filter(schema::option_library::id.eq(id))
        .first(conn)
        .optional()?)
}

pub fn library_type_named(conn: &mut Connection, guild_id: i64, name: &str) -> Result<Option<LibraryOption>, OpError> {
    Ok(LibraryOption::query()
        .filter(schema::option_library::guild_id.eq(guild_id))
        .filter(schema::lower(schema::option_library::name).eq(name.trim().to_lowercase()))
        .first(conn)
        .optional()?)
}

pub fn library_type_by_public(conn: &mut Connection, guild_id: i64, public_id: &str) -> Result<Option<LibraryOption>, OpError> {
    let Ok(public_id) = Uuid::parse_str(public_id.trim()) else {
        return Ok(None);
    };
    Ok(LibraryOption::query()
        .filter(schema::option_library::guild_id.eq(guild_id))
        .filter(schema::option_library::public_id.eq(public_id))
        .first(conn)
        .optional()?)
}

/// Where a library type is used: events still open, presets, and series or routine templates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypeUsage {
    pub events: i64,
    pub presets: i64,
    pub templates: i64,
}

impl TypeUsage {
    pub fn total(&self) -> i64 {
        self.events + self.presets + self.templates
    }

    /// "3 events and 1 preset", for a refusal.
    pub fn describe(&self) -> String {
        self.describe_in(crate::i18n::Lang::En)
    }

    pub fn describe_in(&self, lang: crate::i18n::Lang) -> String {
        let mut parts = Vec::new();
        if self.events > 0 {
            parts.push(crate::tr!(lang, "usage-events", count = self.events));
        }
        if self.presets > 0 {
            parts.push(crate::tr!(lang, "usage-presets", count = self.presets));
        }
        if self.templates > 0 {
            parts.push(crate::tr!(lang, "usage-templates", count = self.templates));
        }
        if parts.is_empty() {
            return crate::tr!(lang, "usage-nothing");
        }
        crate::i18n::list(lang, &parts)
    }
}

/// Every library type of a server that is in use, and where.
pub fn type_usage(conn: &mut Connection, guild_id: i64) -> Result<std::collections::HashMap<i64, TypeUsage>, OpError> {
    let mut usage: std::collections::HashMap<i64, TypeUsage> = std::collections::HashMap::new();
    let events: Vec<(i64, i64)> = schema::signup_options::table
        .inner_join(schema::events::table)
        .filter(schema::events::guild_id.eq(guild_id))
        .group_by(schema::signup_options::type_id)
        .select((schema::signup_options::type_id, diesel::dsl::count_star()))
        .load(conn)?;
    for (type_id, count) in events {
        usage.entry(type_id).or_default().events = count;
    }
    let presets: Vec<(i64, i64)> = schema::preset_options::table
        .inner_join(schema::presets::table)
        .filter(schema::presets::guild_id.eq(guild_id))
        .group_by(schema::preset_options::type_id)
        .select((schema::preset_options::type_id, diesel::dsl::count_star()))
        .load(conn)?;
    for (type_id, count) in presets {
        usage.entry(type_id).or_default().presets = count;
    }
    let templates: Vec<(i64, i64)> = schema::template_options::table
        .inner_join(schema::event_templates::table)
        .filter(schema::event_templates::guild_id.eq(guild_id))
        .group_by(schema::template_options::type_id)
        .select((schema::template_options::type_id, diesel::dsl::count_star()))
        .load(conn)?;
    for (type_id, count) in templates {
        usage.entry(type_id).or_default().templates = count;
    }
    Ok(usage)
}

pub fn settings(conn: &mut Connection, guild_id: i64) -> Result<Settings, OpError> {
    let row: SettingsRow = SettingsRow::query()
        .filter(schema::settings::guild_id.eq(guild_id))
        .first(conn)
        .optional()?
        .ok_or_else(|| OpError::message("This server has no settings yet."))?;
    let mut settings = Settings::from(row);
    let granted: Vec<(String, i64)> = schema::permission_roles::table
        .filter(schema::permission_roles::guild_id.eq(guild_id))
        .order(schema::permission_roles::role_id.asc())
        .select((schema::permission_roles::kind, schema::permission_roles::role_id))
        .load(conn)?;
    for (kind, role_id) in granted {
        if PERMISSION_KINDS.contains(&kind.as_str()) {
            settings.permission_mut(&kind).push(role_id);
        }
    }
    Ok(settings)
}

#[derive(Insertable)]
#[diesel(table_name = schema::permission_roles)]
struct NewPermissionRole<'a> {
    guild_id: i64,
    kind: &'a str,
    role_id: i64,
}

/// Saves the settings row and replaces the roles each permission names.
pub fn update_settings(conn: &mut Connection, settings: &Settings) -> Result<(), OpError> {
    let row = settings.row();
    diesel::update(&row).set(&row).execute(conn)?;
    diesel::delete(
        schema::permission_roles::table.filter(schema::permission_roles::guild_id.eq(settings.guild_id)),
    )
    .execute(conn)?;
    let mut rows = Vec::new();
    for kind in PERMISSION_KINDS {
        let mut ids = settings.permission(kind).to_vec();
        ids.sort_unstable();
        ids.dedup();
        rows.extend(ids.into_iter().map(|role_id| NewPermissionRole {
            guild_id: settings.guild_id,
            kind,
            role_id,
        }));
    }
    if !rows.is_empty() {
        diesel::insert_into(schema::permission_roles::table)
            .values(&rows)
            .execute(conn)?;
    }
    Ok(())
}

pub fn set_guild_timezone(conn: &mut Connection, guild_id: i64, timezone: &str) -> Result<(), OpError> {
    diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(guild_id)))
        .set(schema::guilds::timezone.eq(timezone))
        .execute(conn)?;
    Ok(())
}

/// The server's roles that still exist in Discord.
pub fn list_roles(conn: &mut Connection, guild_id: i64) -> Result<Vec<Role>, OpError> {
    Ok(Role::in_guild(guild_id)
        .filter(schema::roles::gone_at.is_null())
        .order(schema::roles::id.asc())
        .load(conn)?)
}

pub fn role_by_discord(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<Role>, OpError> {
    Ok(Role::in_guild(guild_id)
        .filter(schema::roles::discord_id.eq(discord_id))
        .first(conn)
        .optional()?)
}

pub fn role(conn: &mut Connection, id: i64) -> Result<Option<Role>, OpError> {
    Ok(Role::by_id(id).first(conn).optional()?)
}

pub fn role_by_name(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
) -> Result<Option<Role>, OpError> {
    Ok(Role::named(guild_id, name).first(conn).optional()?)
}

/// The server's channels that still exist, for picking where to post.
pub fn list_channels(conn: &mut Connection, guild_id: i64) -> Result<Vec<Channel>, OpError> {
    Ok(Channel::in_guild(guild_id)
        .filter(schema::channels::gone_at.is_null())
        .order((schema::channels::position.asc(), schema::channels::id.asc()))
        .load(conn)?)
}

/// Every channel the server has had, including deleted ones events still point at.
pub fn all_channels(conn: &mut Connection, guild_id: i64) -> Result<Vec<Channel>, OpError> {
    Ok(Channel::in_guild(guild_id)
        .order((schema::channels::position.asc(), schema::channels::id.asc()))
        .load(conn)?)
}

pub fn channel(conn: &mut Connection, id: i64) -> Result<Option<Channel>, OpError> {
    Ok(Channel::by_id(id).first(conn).optional()?)
}

pub fn channel_by_name(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
) -> Result<Option<Channel>, OpError> {
    let name = name.trim_start_matches('#');
    Ok(Channel::named(guild_id, name).first(conn).optional()?)
}

pub fn channel_by_discord(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<Channel>, OpError> {
    Ok(Channel::by_discord(guild_id, discord_id)
        .first(conn)
        .optional()?)
}

pub fn list_memberships(conn: &mut Connection, guild_id: i64) -> Result<Vec<Membership>, OpError> {
    Ok(Membership::in_guild(guild_id)
        .order(schema::memberships::name.asc())
        .load(conn)?)
}

pub fn membership(conn: &mut Connection, id: i64) -> Result<Option<Membership>, OpError> {
    Ok(Membership::by_id(id).first(conn).optional()?)
}

pub fn membership_by_handle(
    conn: &mut Connection,
    guild_id: i64,
    handle: &str,
) -> Result<Option<Membership>, OpError> {
    let handle = handle.trim_start_matches('@');
    Ok(Membership::by_handle(guild_id, handle)
        .first(conn)
        .optional()?)
}

#[derive(Insertable)]
#[diesel(table_name = schema::users)]
struct NewUser<'a> {
    discord_id: &'a str,
}

pub fn ensure_user(conn: &mut Connection, discord_id: &str) -> Result<i64, OpError> {
    diesel::insert_into(schema::users::table)
        .values(&NewUser { discord_id })
        .on_conflict(schema::users::discord_id)
        .do_nothing()
        .execute(conn)?;
    schema::users::table
        .filter(schema::users::discord_id.eq(discord_id))
        .select(schema::users::id)
        .first(conn)
        .map_err(Into::into)
}

pub fn discord_id_for_membership(conn: &mut Connection, membership_id: i64) -> Result<String, OpError> {
    schema::memberships::table
        .inner_join(schema::users::table)
        .filter(schema::memberships::id.eq(membership_id))
        .select(schema::users::discord_id)
        .first(conn)
        .optional()?
        .ok_or(OpError::NotFound)
}

pub fn membership_by_discord(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<Membership>, OpError> {
    Ok(schema::memberships::table
        .inner_join(schema::users::table)
        .filter(schema::memberships::guild_id.eq(guild_id))
        .filter(schema::users::discord_id.eq(discord_id))
        .select(Membership::as_select())
        .first(conn)
        .optional()?)
}

pub fn membership_role_ids(
    conn: &mut Connection,
    membership: &Membership,
) -> Result<Vec<i64>, OpError> {
    let roles: Vec<Role> = MembershipRole::belonging_to(membership)
        .inner_join(schema::roles::table)
        .select(Role::as_select())
        .load(conn)?;
    Ok(roles.into_iter().map(|role| role.id).collect())
}

pub fn membership_has_role(conn: &mut Connection, membership_id: i64, role_id: i64) -> Result<bool, OpError> {
    let n: i64 = schema::membership_roles::table
        .filter(schema::membership_roles::membership_id.eq(membership_id))
        .filter(schema::membership_roles::role_id.eq(role_id))
        .count()
        .get_result(conn)?;
    Ok(n > 0)
}

pub fn replace_discord_roles(
    conn: &mut Connection,
    guild_id: i64,
    membership_id: i64,
    discord_role_ids: &[String],
) -> Result<(), OpError> {
    let known = list_roles(conn, guild_id)?;
    let role_ids: Vec<i64> = known
        .into_iter()
        .filter(|role| {
            role.discord_id.as_deref().is_some_and(|id| {
                discord_role_ids.iter().any(|wanted| wanted == id)
            })
        })
        .map(|role| role.id)
        .collect();
    // Requests and the gateway may replace the same member's roles at once:
    // the transaction keeps readers from seeing them roleless in between, and
    // the insert skips rows another replacement has just written.
    conn.transaction(|conn| {
        diesel::delete(schema::membership_roles::table.filter(schema::membership_roles::membership_id.eq(membership_id)))
            .execute(conn)?;
        insert_membership_roles(conn, membership_id, &role_ids)
    })
}

#[derive(Insertable)]
#[diesel(table_name = schema::membership_roles)]
struct NewMembershipRole {
    membership_id: i64,
    role_id: i64,
}

pub fn insert_membership_roles(
    conn: &mut Connection,
    membership_id: i64,
    role_ids: &[i64],
) -> Result<(), OpError> {
    let mut ids = role_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let rows: Vec<_> = ids
        .into_iter()
        .map(|role_id| NewMembershipRole {
            membership_id,
            role_id,
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::membership_roles::table)
        .values(&rows)
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

pub fn event(conn: &mut Connection, id: i64) -> Result<Option<Event>, OpError> {
    Ok(Event::by_id(id).first(conn).optional()?)
}

pub fn event_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Event>, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Ok(None);
    };
    Ok(Event::by_public(public_id).first(conn).optional()?)
}

pub fn discord_server_id(conn: &mut Connection, guild_id: i64) -> Result<String, OpError> {
    let guild = guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    guild
        .discord_id
        .filter(|id| !id.is_empty())
        .ok_or(OpError::NotFound)
}

pub fn list_events(conn: &mut Connection, guild_id: i64) -> Result<Vec<Event>, OpError> {
    Ok(Event::in_guild(guild_id)
        .order((schema::events::starts_at.asc(), schema::events::id.asc()))
        .load(conn)?)
}

pub fn events_in_series(conn: &mut Connection, series_id: i64) -> Result<Vec<Event>, OpError> {
    Ok(Event::in_series(series_id)
        .order((
            schema::events::occurrence_index.asc(),
            schema::events::id.asc(),
        ))
        .load(conn)?)
}

pub fn list_series(conn: &mut Connection, guild_id: i64) -> Result<Vec<Series>, OpError> {
    Ok(Series::in_guild(guild_id)
        .order(schema::series::id.desc())
        .load(conn)?)
}

pub fn series(conn: &mut Connection, id: i64) -> Result<Option<Series>, OpError> {
    Ok(Series::by_id(id).first(conn).optional()?)
}

pub fn series_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Series>, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Ok(None);
    };
    Ok(Series::by_public(public_id).first(conn).optional()?)
}

pub fn options_for(conn: &mut Connection, event: &Event) -> Result<Vec<SignupOption>, OpError> {
    load_options(conn, OptionsOf::Events(&[event.id]))
}

enum OptionsOf<'a> {
    Events(&'a [i64]),
    One(i64),
}

/// Options with their library type, in each event's order.
fn load_options(conn: &mut Connection, of: OptionsOf<'_>) -> Result<Vec<SignupOption>, OpError> {
    let mut query = schema::signup_options::table
        .inner_join(schema::option_library::table)
        .select((
            SignupOptionRow::as_select(),
            (
                schema::option_library::name,
                schema::option_library::emoji,
                schema::option_library::attending,
                schema::option_library::public_id,
            ),
        ))
        .order((schema::signup_options::position.asc(), schema::signup_options::id.asc()))
        .into_boxed();
    query = match of {
        OptionsOf::Events(ids) => query.filter(schema::signup_options::event_id.eq_any(ids)),
        OptionsOf::One(id) => query.filter(schema::signup_options::id.eq(id)),
    };
    let rows: Vec<(SignupOptionRow, (String, String, bool, Uuid))> = query.load(conn)?;
    Ok(rows
        .into_iter()
        .map(|(row, (name, emoji, attending, type_public_id))| SignupOption {
            id: row.id,
            event_id: row.event_id,
            type_id: row.type_id,
            type_public_id,
            name,
            emoji,
            capacity: row.capacity,
            max_guests: row.max_guests,
            role_id: row.role_id,
            reminders_enabled: attending,
            position: row.position,
            public_id: row.public_id,
        })
        .collect())
}

pub fn options_by_event(
    conn: &mut Connection,
    events: &[Event],
) -> Result<Vec<Vec<SignupOption>>, OpError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = events.iter().map(|event| event.id).collect();
    let rows = load_options(conn, OptionsOf::Events(&ids))?;
    Ok(events
        .iter()
        .map(|event| rows.iter().filter(|opt| opt.event_id == event.id).cloned().collect())
        .collect())
}

pub fn option(conn: &mut Connection, id: i64) -> Result<Option<SignupOption>, OpError> {
    Ok(load_options(conn, OptionsOf::One(id))?.into_iter().next())
}

pub fn signups_for(conn: &mut Connection, event: &Event) -> Result<Vec<Signup>, OpError> {
    Ok(Signup::for_event(event.id).load(conn)?)
}

pub fn signups_by_event(
    conn: &mut Connection,
    events: &[Event],
) -> Result<Vec<Vec<Signup>>, OpError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = events.iter().map(|event| event.id).collect();
    let rows = Signup::for_events(&ids).load(conn)?;
    Ok(rows.grouped_by(events))
}

pub fn reminders_for(conn: &mut Connection, event: &Event) -> Result<Vec<Reminder>, OpError> {
    Ok(Reminder::for_event(event.id).load(conn)?)
}

pub fn reminders_by_event(
    conn: &mut Connection,
    events: &[Event],
) -> Result<Vec<Vec<Reminder>>, OpError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = events.iter().map(|event| event.id).collect();
    let rows = Reminder::for_events(&ids).load(conn)?;
    Ok(rows.grouped_by(events))
}

pub fn events_in_active_series(
    conn: &mut Connection,
    guild_id: i64,
) -> Result<Vec<Event>, OpError> {
    Ok(schema::events::table
        .inner_join(schema::series::table)
        .filter(schema::events::guild_id.eq(guild_id))
        .filter(schema::series::active.eq(true))
        .order((
            schema::events::series_id.asc(),
            schema::events::occurrence_index.asc(),
            schema::events::id.asc(),
        ))
        .select(Event::as_select())
        .load(conn)?)
}

pub fn event_role_ids(
    conn: &mut Connection,
    event: &Event,
    kind: &str,
) -> Result<Vec<i64>, OpError> {
    let roles: Vec<Role> = EventRole::belonging_to(event)
        .filter(schema::event_roles::kind.eq(kind))
        .inner_join(schema::roles::table)
        .select(Role::as_select())
        .load(conn)?;
    Ok(roles.into_iter().map(|role| role.id).collect())
}

/// A server's activity, or only one of its events'.
/// The server's activity, or one event's, leaving out the `hidden` events'.
pub fn count_activity(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    hidden: &[i64],
) -> Result<i64, OpError> {
    let mut query = Activity::in_guild(guild_id).into_boxed();
    if let Some(event_id) = event_id {
        query = query.filter(schema::activity::event_id.eq(event_id));
    }
    if !hidden.is_empty() {
        query = query.filter(
            schema::activity::event_id
                .is_null()
                .or(schema::activity::event_id.ne_all(hidden.to_vec())),
        );
    }
    Ok(query.count().get_result(conn)?)
}

pub fn list_activity(
    conn: &mut Connection,
    guild_id: i64,
    event_id: Option<i64>,
    hidden: &[i64],
    limit: i64,
    offset: i64,
) -> Result<Vec<Activity>, OpError> {
    let mut query = Activity::in_guild(guild_id).into_boxed();
    if let Some(event_id) = event_id {
        query = query.filter(schema::activity::event_id.eq(event_id));
    }
    if !hidden.is_empty() {
        query = query.filter(
            schema::activity::event_id
                .is_null()
                .or(schema::activity::event_id.ne_all(hidden.to_vec())),
        );
    }
    Ok(query
        .order(schema::activity::id.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)?)
}

pub fn list_presets(conn: &mut Connection, guild_id: i64) -> Result<Vec<Preset>, OpError> {
    Ok(Preset::in_guild(guild_id)
        .order(schema::presets::name.asc())
        .load(conn)?)
}

pub fn preset(conn: &mut Connection, id: i64) -> Result<Option<Preset>, OpError> {
    Ok(Preset::by_id(id).first(conn).optional()?)
}

pub fn preset_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Preset>, OpError> {
    let Some(public_id) = crate::ids::parse_event_id(public_id) else {
        return Ok(None);
    };
    Ok(Preset::by_public(public_id).first(conn).optional()?)
}

pub fn list_library(conn: &mut Connection, guild_id: i64) -> Result<Vec<LibraryOption>, OpError> {
    Ok(LibraryOption::query()
        .filter(schema::option_library::guild_id.eq(guild_id))
        .order(schema::option_library::name.asc())
        .load(conn)?)
}

pub fn thread_member_ids(conn: &mut Connection, event: &Event) -> Result<Vec<i64>, OpError> {
    let members: Vec<Membership> = ThreadMember::belonging_to(event)
        .inner_join(schema::memberships::table)
        .select(Membership::as_select())
        .load(conn)?;
    Ok(members.into_iter().map(|member| member.id).collect())
}

pub fn grant_membership_ids(conn: &mut Connection, event: &Event) -> Result<Vec<i64>, OpError> {
    let members: Vec<Membership> = RoleGrant::belonging_to(event)
        .inner_join(schema::memberships::table)
        .select(Membership::as_select())
        .load(conn)?;
    Ok(members.into_iter().map(|member| member.id).collect())
}

pub fn thread_discord_id(conn: &mut Connection, event_id: i64) -> Result<Option<String>, OpError> {
    Ok(schema::threads::table
        .filter(schema::threads::event_id.eq(event_id))
        .select(schema::threads::discord_id)
        .first(conn)
        .optional()?
        .flatten())
}

/// Remembers an event's Discord thread, whether or not Marian kept a row for it yet.
pub fn set_thread_discord_id(
    conn: &mut Connection,
    event_id: i64,
    name: &str,
    discord_id: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::threads::table)
        .values((
            schema::threads::event_id.eq(event_id),
            schema::threads::name.eq(name),
            schema::threads::discord_id.eq(discord_id),
        ))
        .on_conflict(schema::threads::event_id)
        .do_update()
        .set(schema::threads::discord_id.eq(discord_id))
        .execute(conn)?;
    Ok(())
}

pub fn set_role_discord_id(
    conn: &mut Connection,
    role_id: i64,
    discord_id: &str,
) -> Result<(), OpError> {
    diesel::update(schema::roles::table.filter(schema::roles::id.eq(role_id)))
        .set(schema::roles::discord_id.eq(discord_id))
        .execute(conn)?;
    Ok(())
}

pub fn discord_ids_for_memberships(
    conn: &mut Connection,
    membership_ids: &[i64],
) -> Result<Vec<String>, OpError> {
    let mut ids: Vec<String> = schema::memberships::table
        .inner_join(schema::users::table)
        .filter(schema::memberships::id.eq_any(membership_ids))
        .select(schema::users::discord_id)
        .load(conn)?;
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn applied_membership_ids(
    conn: &mut Connection,
    event_id: i64,
    kind: &str,
) -> Result<Vec<String>, OpError> {
    Ok(schema::applied_access::table
        .filter(schema::applied_access::event_id.eq(event_id))
        .filter(schema::applied_access::kind.eq(kind))
        .order(schema::applied_access::discord_user_id.asc())
        .select(schema::applied_access::discord_user_id)
        .load(conn)?)
}

#[derive(Insertable)]
#[diesel(table_name = schema::applied_access)]
struct NewApplied<'a> {
    event_id: i64,
    discord_user_id: &'a str,
    kind: &'a str,
}

pub fn mark_applied(
    conn: &mut Connection,
    event_id: i64,
    kind: &str,
    discord_user_id: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::applied_access::table)
        .values(&NewApplied {
            event_id,
            discord_user_id,
            kind,
        })
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

pub fn clear_applied(
    conn: &mut Connection,
    event_id: i64,
    kind: &str,
    discord_user_id: &str,
) -> Result<(), OpError> {
    diesel::delete(
        schema::applied_access::table
            .filter(schema::applied_access::event_id.eq(event_id))
            .filter(schema::applied_access::kind.eq(kind))
            .filter(schema::applied_access::discord_user_id.eq(discord_user_id)),
    )
    .execute(conn)?;
    Ok(())
}

pub fn list_commands(
    conn: &mut Connection,
    guild_id: i64,
    limit: i64,
) -> Result<Vec<CommandRow>, OpError> {
    Ok(CommandRow::query()
        .filter(schema::command_log::guild_id.eq(guild_id))
        .order(schema::command_log::id.desc())
        .limit(limit)
        .load(conn)?)
}

pub fn count_status(conn: &mut Connection, guild_id: i64, status: &str) -> Result<i64, OpError> {
    Ok(schema::events::table
        .filter(schema::events::guild_id.eq(guild_id))
        .filter(schema::events::status.eq(status))
        .count()
        .get_result(conn)?)
}

/// Posted events that have not ended, plus scheduled posts.
pub fn count_upcoming(conn: &mut Connection, guild_id: i64) -> Result<i64, OpError> {
    let now = Utc::now();
    let rows: Vec<(String, DateTime<Utc>, i64)> = schema::events::table
        .filter(schema::events::guild_id.eq(guild_id))
        .filter(schema::events::status.eq_any(["posted", "scheduled"]))
        .select((
            schema::events::status,
            schema::events::starts_at,
            schema::events::duration_minutes,
        ))
        .load(conn)?;
    Ok(rows
        .into_iter()
        .filter(|(status, starts, minutes)| match status.as_str() {
            "posted" => *starts + chrono::Duration::minutes(*minutes) >= now,
            "scheduled" => true,
            _ => false,
        })
        .count() as i64)
}

pub fn count_active_series(conn: &mut Connection, guild_id: i64) -> Result<i64, OpError> {
    Ok(schema::series::table
        .filter(schema::series::guild_id.eq(guild_id))
        .filter(schema::series::active.eq(true))
        .filter(schema::series::cadence.ne("once"))
        .count()
        .get_result(conn)?)
}

pub struct WebSession {
    pub token: String,
    pub discord_user_id: String,
    pub display_name: String,
    pub guild_id: i64,
    pub membership_id: i64,
    pub created_at: DateTime<Utc>,
    pub guilds: Vec<i64>,
    pub grant: Option<DiscordGrant>,
    pub guilds_checked_at: DateTime<Utc>,
}

/// The member's Discord OAuth grant, kept to re-read their server list.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscordGrant {
    pub access: String,
    pub refresh: String,
    pub expires_at: DateTime<Utc>,
}

/// How long a web session lasts after sign-in.
pub const SESSION_DAYS: i64 = 30;

#[derive(Insertable)]
#[diesel(table_name = schema::web_sessions)]
struct NewWebSession<'a> {
    token: &'a str,
    discord_user_id: &'a str,
    display_name: &'a str,
    guild_id: i64,
    membership_id: i64,
    created_at: DateTime<Utc>,
    discord_access: Option<&'a str>,
    discord_refresh: Option<&'a str>,
    discord_expires_at: Option<DateTime<Utc>>,
}

#[allow(clippy::too_many_arguments)]
pub fn insert_web_session(
    conn: &mut Connection,
    token: &str,
    discord_user_id: &str,
    display_name: &str,
    guild_id: i64,
    membership_id: i64,
    guilds: &[i64],
    grant: Option<&DiscordGrant>,
) -> Result<(), OpError> {
    let token = token.to_string();
    let discord_user_id = discord_user_id.to_string();
    let display_name = display_name.to_string();
    let guilds = guilds.to_vec();
    conn.transaction(|conn| {
        diesel::insert_into(schema::web_sessions::table)
            .values(&NewWebSession {
                token: &token,
                discord_user_id: &discord_user_id,
                display_name: &display_name,
                guild_id,
                membership_id,
                created_at: Utc::now(),
                discord_access: grant.map(|grant| grant.access.as_str()),
                discord_refresh: grant.map(|grant| grant.refresh.as_str()),
                discord_expires_at: grant.map(|grant| grant.expires_at),
            })
            .execute(conn)?;
        insert_session_guilds(conn, &token, &guilds)?;
        Ok(())
    })
}

#[derive(Insertable)]
#[diesel(table_name = schema::web_session_guilds)]
struct NewSessionGuild<'a> {
    token: &'a str,
    guild_id: i64,
}

fn insert_session_guilds(
    conn: &mut Connection,
    token: &str,
    guilds: &[i64],
) -> Result<(), OpError> {
    let rows: Vec<_> = guilds
        .iter()
        .map(|guild_id| NewSessionGuild {
            token,
            guild_id: *guild_id,
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    diesel::insert_into(schema::web_session_guilds::table)
        .values(&rows)
        .execute(conn)?;
    Ok(())
}

pub fn web_session(conn: &mut Connection, token: &str) -> Result<Option<WebSession>, OpError> {
    let Some(row) = SessionRow::query()
        .filter(schema::web_sessions::token.eq(token))
        .first(conn)
        .optional()?
    else {
        return Ok(None);
    };
    let guilds = SessionGuild::belonging_to(&row)
        .select(SessionGuild::as_select())
        .load(conn)?
        .into_iter()
        .map(|link| link.guild_id)
        .collect();
    let grant = match (row.discord_access, row.discord_refresh, row.discord_expires_at) {
        (Some(access), Some(refresh), Some(expires_at)) => Some(DiscordGrant {
            access,
            refresh,
            expires_at,
        }),
        _ => None,
    };
    Ok(Some(WebSession {
        token: row.token,
        discord_user_id: row.discord_user_id,
        display_name: row.display_name,
        guild_id: row.guild_id,
        membership_id: row.membership_id,
        created_at: row.created_at,
        guilds,
        grant,
        guilds_checked_at: row.guilds_checked_at,
    }))
}

/// Notes that the session's servers were just looked at, so a burst of
/// requests asks Discord once.
pub fn touch_session_guilds(conn: &mut Connection, token: &str) -> Result<(), OpError> {
    diesel::update(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .set(schema::web_sessions::guilds_checked_at.eq(Utc::now()))
        .execute(conn)?;
    Ok(())
}

/// Replaces the session's grant; `None` forgets it once Discord stops honouring it.
pub fn set_session_grant(
    conn: &mut Connection,
    token: &str,
    grant: Option<&DiscordGrant>,
) -> Result<(), OpError> {
    diesel::update(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .set((
            schema::web_sessions::discord_access.eq(grant.map(|grant| grant.access.as_str())),
            schema::web_sessions::discord_refresh.eq(grant.map(|grant| grant.refresh.as_str())),
            schema::web_sessions::discord_expires_at.eq(grant.map(|grant| grant.expires_at)),
        ))
        .execute(conn)?;
    Ok(())
}

/// Swaps the session's servers for `guilds`, landing on `home` when the one it
/// pointed at is no longer among them.
pub fn replace_session_guilds(
    conn: &mut Connection,
    token: &str,
    guilds: &[i64],
    home: (i64, i64),
) -> Result<(), OpError> {
    conn.transaction(|conn| {
        diesel::delete(
            schema::web_session_guilds::table.filter(schema::web_session_guilds::token.eq(token)),
        )
        .execute(conn)?;
        insert_session_guilds(conn, token, guilds)?;
        let current: i64 = schema::web_sessions::table
            .filter(schema::web_sessions::token.eq(token))
            .select(schema::web_sessions::guild_id)
            .first(conn)?;
        let sessions = schema::web_sessions::table.filter(schema::web_sessions::token.eq(token));
        if guilds.contains(&current) {
            diesel::update(sessions)
                .set(schema::web_sessions::guilds_checked_at.eq(Utc::now()))
                .execute(conn)?;
        } else {
            diesel::update(sessions)
                .set((
                    schema::web_sessions::guild_id.eq(home.0),
                    schema::web_sessions::membership_id.eq(home.1),
                    schema::web_sessions::guilds_checked_at.eq(Utc::now()),
                ))
                .execute(conn)?;
        }
        Ok(())
    })
}

/// Live sessions that can't open this server yet, as (token, Discord user id,
/// display name); only `user`'s when given.
pub fn sessions_without_guild(
    conn: &mut Connection,
    guild_id: i64,
    user: Option<&str>,
) -> Result<Vec<(String, String, String)>, OpError> {
    let has_it = schema::web_session_guilds::table
        .filter(schema::web_session_guilds::guild_id.eq(guild_id))
        .select(schema::web_session_guilds::token);
    let mut query = schema::web_sessions::table
        .filter(schema::web_sessions::created_at.gt(Utc::now() - chrono::Duration::days(SESSION_DAYS)))
        .filter(diesel::dsl::not(schema::web_sessions::token.eq_any(has_it)))
        .into_boxed();
    if let Some(user) = user {
        query = query.filter(schema::web_sessions::discord_user_id.eq(user.to_string()));
    }
    Ok(query
        .select((
            schema::web_sessions::token,
            schema::web_sessions::discord_user_id,
            schema::web_sessions::display_name,
        ))
        .order(schema::web_sessions::discord_user_id.asc())
        .load(conn)?)
}

/// Takes a server out of the member's sessions once they leave it. A session
/// that was on it moves to another of its servers, or ends with none left.
pub fn drop_session_guild(conn: &mut Connection, discord_user_id: &str, guild_id: i64) -> Result<(), OpError> {
    conn.transaction(|conn| {
        let tokens: Vec<String> = schema::web_sessions::table
            .filter(schema::web_sessions::discord_user_id.eq(discord_user_id))
            .select(schema::web_sessions::token)
            .load(conn)?;
        diesel::delete(
            schema::web_session_guilds::table
                .filter(schema::web_session_guilds::token.eq_any(&tokens))
                .filter(schema::web_session_guilds::guild_id.eq(guild_id)),
        )
        .execute(conn)?;
        let stranded: Vec<String> = schema::web_sessions::table
            .filter(schema::web_sessions::token.eq_any(&tokens))
            .filter(schema::web_sessions::guild_id.eq(guild_id))
            .select(schema::web_sessions::token)
            .load(conn)?;
        for token in stranded {
            let next = schema::web_session_guilds::table
                .inner_join(
                    schema::memberships::table
                        .on(schema::memberships::guild_id.eq(schema::web_session_guilds::guild_id)),
                )
                .inner_join(schema::users::table.on(schema::users::id.eq(schema::memberships::user_id)))
                .filter(schema::web_session_guilds::token.eq(&token))
                .filter(schema::users::discord_id.eq(discord_user_id))
                .order(schema::web_session_guilds::guild_id.asc())
                .select((schema::web_session_guilds::guild_id, schema::memberships::id))
                .first::<(i64, i64)>(conn)
                .optional()?;
            let session = schema::web_sessions::table.filter(schema::web_sessions::token.eq(&token));
            match next {
                Some((next_guild, membership_id)) => {
                    diesel::update(session)
                        .set((
                            schema::web_sessions::guild_id.eq(next_guild),
                            schema::web_sessions::membership_id.eq(membership_id),
                        ))
                        .execute(conn)?;
                }
                None => {
                    diesel::delete(session).execute(conn)?;
                }
            }
        }
        Ok(())
    })
}

pub fn add_session_guild(conn: &mut Connection, token: &str, guild_id: i64) -> Result<(), OpError> {
    diesel::insert_into(schema::web_session_guilds::table)
        .values(&NewSessionGuild { token, guild_id })
        .on_conflict_do_nothing()
        .execute(conn)?;
    Ok(())
}

pub fn delete_web_session(conn: &mut Connection, token: &str) -> Result<(), OpError> {
    diesel::delete(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = schema::calendar_feeds)]
struct NewCalendarFeed<'a> {
    token: &'a str,
    user_id: i64,
    created_at: DateTime<Utc>,
}

pub fn ensure_calendar_token(conn: &mut Connection, user_id: i64) -> Result<String, OpError> {
    let existing: Option<String> = schema::calendar_feeds::table
        .filter(schema::calendar_feeds::user_id.eq(user_id))
        .select(schema::calendar_feeds::token)
        .first(conn)
        .optional()?;
    if let Some(token) = existing {
        return Ok(token);
    }
    let token = crate::auth::random_token()?;
    diesel::insert_into(schema::calendar_feeds::table)
        .values(&NewCalendarFeed {
            token: &token,
            user_id,
            created_at: Utc::now(),
        })
        .execute(conn)?;
    Ok(token)
}

pub fn calendar_user_id(conn: &mut Connection, token: &str) -> Result<Option<i64>, OpError> {
    Ok(schema::calendar_feeds::table
        .filter(schema::calendar_feeds::token.eq(token))
        .select(schema::calendar_feeds::user_id)
        .first(conn)
        .optional()?)
}

#[cfg(test)]
mod tests {
    use diesel::prelude::{Insertable, QueryableByName, RunQueryDsl};
    use diesel::sql_query;

    use super::*;

    #[derive(Insertable)]
    #[diesel(table_name = schema::guilds)]
    struct SeedGuild<'a> {
        id: i64,
        name: &'a str,
        timezone: &'a str,
    }

    #[test]
    fn a_connection_string_names_the_database() {
        let url = parse_database_url("postgres://marian:marian@127.0.0.1/marian").unwrap();
        assert_eq!(url, "postgres://marian:marian@127.0.0.1/marian");

        let url = parse_database_url("  PostgreSQL://marian@localhost/marian ").unwrap();
        assert_eq!(url, "postgresql://marian@localhost/marian");

        let err = parse_database_url("sqlite://data/marian.db").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("sqlite"));
        assert!(!text.contains("marian.db"));

        let err = parse_database_url("mysql://user:secret@localhost/marian").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("mysql"));
        assert!(!text.contains("secret"));

        assert!(parse_database_url("").is_err());
        assert!(parse_database_url("/data/marian.db").is_err());
    }

    #[test]
    fn dotenv_lines_keep_the_value_after_the_first_equals() {
        assert_eq!(
            parse_dotenv_line("  export DATABASE_URL=postgres://marian:marian@127.0.0.1/marian  "),
            Some((
                "DATABASE_URL".to_string(),
                "postgres://marian:marian@127.0.0.1/marian".to_string()
            ))
        );
        assert_eq!(
            parse_dotenv_line("DISCORD_TOKEN=\"bot.token\""),
            Some(("DISCORD_TOKEN".to_string(), "bot.token".to_string()))
        );
        assert_eq!(parse_dotenv_line("# PORT=1"), None);
        assert_eq!(parse_dotenv_line("PORT="), None);
    }

    #[test]
    fn open_applies_the_migrations() {
        let mut conn = open_memory().unwrap();
        diesel::insert_into(schema::guilds::table)
            .values(&SeedGuild {
                id: 1,
                name: "Guild",
                timezone: "UTC",
            })
            .execute(&mut conn)
            .unwrap();
        migrate(&mut conn).unwrap();
        let guilds = list_guilds(&mut conn).unwrap();
        assert_eq!(guilds.len(), 1);
        assert_eq!(guilds[0].name, "Guild");
        #[derive(QueryableByName)]
        struct VersionRow {
            #[diesel(sql_type = diesel::sql_types::Text)]
            version: String,
        }
        let versions = sql_query("SELECT version FROM __diesel_schema_migrations")
            .load::<VersionRow>(&mut conn)
            .unwrap();
        let mut versions: Vec<_> = versions.into_iter().map(|row| row.version).collect();
        versions.sort();
        assert_eq!(
            versions,
            vec![
                "20261015131400".to_string(),
                "20261015164600".to_string(),
                "20261015170700".to_string(),
                "20261017033000".to_string(),
                "20261017040000".to_string(),
                "20261017053000".to_string(),
                "20261017060000".to_string(),
                "20261017070000".to_string(),
                "20261017080000".to_string(),
                "20261017090000".to_string(),
                "20261017100000".to_string(),
                "20261017160000".to_string(),
                "20261017170000".to_string(),
                "20261018090000".to_string(),
                "20261018100000".to_string(),
                "20261018110000".to_string(),
                "20261019090000".to_string(),
                "20261019130000".to_string(),
                "20261019150000".to_string(),
                "20261019170000".to_string(),
                "20261021090000".to_string(),
                "20261024090000".to_string(),
                "20261024120000".to_string(),
                "20261026090000".to_string(),
                "20261026100000".to_string(),
                "20261026110000".to_string(),
                "20261102090000".to_string(),
                "20261109090000".to_string(),
                "20261110090000".to_string(),
                "20261111100000".to_string(),
            ]
        );
    }

    #[test]
    fn a_discord_role_list_replaces_the_stored_roles() {
        let mut conn = open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO users (discord_id) VALUES ('nell');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233' FROM users WHERE discord_id = 'nell';
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES
              (1, 1, 'Raiders', '#112233', '111'),
              (2, 1, 'Officers', '#112233', '222'),
              (3, 1, 'Local', '#112233', NULL);
            INSERT INTO membership_roles (membership_id, role_id) VALUES (1, 1), (1, 3);
            ",
        )
        .unwrap();
        replace_discord_roles(&mut conn, 1, 1, &["222".into()]).unwrap();
        let person = membership(&mut conn, 1).unwrap().unwrap();
        let roles = membership_role_ids(&mut conn, &person).unwrap();
        assert_eq!(roles, vec![2]);
        // A replacement running alongside writes the same row first.
        insert_membership_roles(&mut conn, 1, &[2]).unwrap();
        assert_eq!(membership_role_ids(&mut conn, &person).unwrap(), vec![2]);
        replace_discord_roles(&mut conn, 1, 1, &[]).unwrap();
        let person = membership(&mut conn, 1).unwrap().unwrap();
        assert!(membership_role_ids(&mut conn, &person).unwrap().is_empty());
    }

    fn two_servers() -> Connection {
        let mut conn = open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone, discord_id) VALUES (1, 'One', 'UTC', '10'), (2, 'Two', 'UTC', '20');
            INSERT INTO users (discord_id) VALUES ('u1');
            INSERT INTO memberships (id, user_id, guild_id, name, handle, color, reminders_enabled, server_manager)
            SELECT 1, id, 1, 'Nell', 'nell', '#112233', true, false FROM users WHERE discord_id = 'u1'
            UNION ALL
            SELECT 2, id, 2, 'Nell', 'nell', '#112233', true, false FROM users WHERE discord_id = 'u1';
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_session_keeps_its_discord_grant() {
        let mut conn = two_servers();
        let grant = DiscordGrant {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at: Utc::now(),
        };
        insert_web_session(&mut conn, "t", "u1", "Nell", 1, 1, &[1], Some(&grant)).unwrap();
        let stored = web_session(&mut conn, "t").unwrap().unwrap().grant.unwrap();
        assert_eq!(stored.access, "access");
        assert_eq!(stored.refresh, "refresh");

        set_session_grant(&mut conn, "t", None).unwrap();
        assert!(web_session(&mut conn, "t").unwrap().unwrap().grant.is_none());
    }

    #[test]
    fn replacing_servers_moves_off_one_the_member_left() {
        let mut conn = two_servers();
        insert_web_session(&mut conn, "t", "u1", "Nell", 1, 1, &[1], None).unwrap();

        replace_session_guilds(&mut conn, "t", &[1, 2], (1, 1)).unwrap();
        let session = web_session(&mut conn, "t").unwrap().unwrap();
        assert_eq!(session.guilds, vec![1, 2]);
        assert_eq!(session.guild_id, 1);

        replace_session_guilds(&mut conn, "t", &[2], (2, 2)).unwrap();
        let session = web_session(&mut conn, "t").unwrap().unwrap();
        assert_eq!(session.guilds, vec![2]);
        assert_eq!((session.guild_id, session.membership_id), (2, 2));
    }

    #[test]
    fn a_new_server_is_offered_to_sessions_without_it() {
        let mut conn = two_servers();
        insert_web_session(&mut conn, "a", "u1", "Nell", 1, 1, &[1], None).unwrap();
        insert_web_session(&mut conn, "b", "u1", "Nell", 1, 1, &[1, 2], None).unwrap();

        let waiting = sessions_without_guild(&mut conn, 2, None).unwrap();
        assert_eq!(waiting, vec![("a".into(), "u1".into(), "Nell".into())]);
        assert!(sessions_without_guild(&mut conn, 2, Some("u2")).unwrap().is_empty());

        add_session_guild(&mut conn, "a", 2).unwrap();
        add_session_guild(&mut conn, "a", 2).unwrap();
        assert_eq!(web_session(&mut conn, "a").unwrap().unwrap().guilds, vec![1, 2]);
        assert!(sessions_without_guild(&mut conn, 2, None).unwrap().is_empty());
    }

    #[test]
    fn leaving_a_server_takes_it_out_of_the_members_sessions() {
        let mut conn = two_servers();
        insert_web_session(&mut conn, "both", "u1", "Nell", 1, 1, &[1, 2], None).unwrap();
        insert_web_session(&mut conn, "only", "u1", "Nell", 1, 1, &[1], None).unwrap();

        drop_session_guild(&mut conn, "u1", 1).unwrap();
        let both = web_session(&mut conn, "both").unwrap().unwrap();
        assert_eq!(both.guilds, vec![2]);
        assert_eq!((both.guild_id, both.membership_id), (2, 2));
        assert!(web_session(&mut conn, "only").unwrap().is_none());
    }

    #[test]
    fn a_role_stored_twice_is_folded_into_one() {
        let mut conn = two_servers();
        // Back to before the migration that folds the duplicates.
        while conn.revert_last_migration(MIGRATIONS).unwrap().to_string() != "20261026110000" {}
        conn.batch_execute(
            "
            INSERT INTO channels (id, guild_id, name, kind, position) VALUES (81, 1, 'raids', 'text', 1);
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES
              (91, 1, 'Raider', '#112233', '700'), (92, 1, 'Raiders', '#112233', '700'), (93, 1, 'Healer', '#112233', '701');
            INSERT INTO membership_roles (membership_id, role_id) VALUES (1, 91), (1, 92), (2, 92);
            INSERT INTO permission_roles (guild_id, kind, role_id) VALUES (1, 'event', 92);
            INSERT INTO event_templates (id, guild_id, channel_id, title, duration_minutes, created_at, mention_role_ids, restrict_role_ids)
            VALUES (95, 1, 81, 'Raid', 60, now(), '{92,93,91}', '{93}');
            ",
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        let ids: Vec<i64> = schema::roles::table
            .order(schema::roles::id.asc())
            .select(schema::roles::id)
            .load(&mut conn)
            .unwrap();
        assert_eq!(ids, vec![91, 93]);
        let held: Vec<(i64, i64)> = schema::membership_roles::table
            .order(schema::membership_roles::membership_id.asc())
            .select((schema::membership_roles::membership_id, schema::membership_roles::role_id))
            .load(&mut conn)
            .unwrap();
        assert_eq!(held, vec![(1, 91), (2, 91)]);
        let granted: Vec<i64> = schema::permission_roles::table
            .select(schema::permission_roles::role_id)
            .load(&mut conn)
            .unwrap();
        assert_eq!(granted, vec![91]);
        let (mention, restrict): (Vec<i64>, Vec<i64>) = schema::event_templates::table
            .select((schema::event_templates::mention_role_ids, schema::event_templates::restrict_role_ids))
            .first(&mut conn)
            .unwrap();
        assert_eq!(mention, vec![91, 93]);
        assert_eq!(restrict, vec![93]);
    }
}
