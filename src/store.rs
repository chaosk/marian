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

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::settings)]
#[diesel(primary_key(guild_id))]
#[diesel(belongs_to(Guild))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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
    pub role_event: Option<i64>,
    pub role_edit: Option<i64>,
    pub role_delete: Option<i64>,
    pub role_channel: Option<i64>,
    pub role_signup: Option<i64>,
    pub post_lead_hours: i64,
    pub digest: String,
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
    pub organizer: bool,
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
    pub(crate) options_json: serde_json::Value,
    pub(crate) reminders_json: serde_json::Value,
    pub(crate) mention_role_ids: Vec<i64>,
    pub(crate) restrict_role_ids: Vec<i64>,
    #[allow(dead_code)]
    pub(crate) created_at: DateTime<Utc>,
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
}

#[derive(Debug, Clone, HasQuery, Identifiable, Associations, AsChangeset)]
#[diesel(table_name = crate::schema::signup_options)]
#[diesel(belongs_to(Event))]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SignupOption {
    pub id: i64,
    pub event_id: i64,
    pub name: String,
    pub emoji: String,
    pub capacity: Option<i64>,
    pub waitlist: bool,
    pub role_id: Option<i64>,
    pub reminders_enabled: bool,
    pub position: i64,
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
    pub body: serde_json::Value,
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
model_query!(SignupOptionQuery, SignupOption);
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

impl SignupOption {
    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn by_id(id: i64) -> _ {
        let options: SignupOptionQuery = SignupOption::query();
        options.filter(schema::signup_options::id.eq(id))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_event(event_id: i64) -> _ {
        let options: SignupOptionQuery = SignupOption::query();
        options
            .filter(schema::signup_options::event_id.eq(event_id))
            .order((
                schema::signup_options::position.asc(),
                schema::signup_options::id.asc(),
            ))
    }

    #[diesel::dsl::auto_type(no_type_alias)]
    pub fn for_events(event_ids: &[i64]) -> _ {
        let options: SignupOptionQuery = SignupOption::query();
        options
            .filter(schema::signup_options::event_id.eq_any(event_ids))
            .order((
                schema::signup_options::position.asc(),
                schema::signup_options::id.asc(),
            ))
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
        auto_join_threads: true,
        show_attendees: true,
        hour_24: false,
        localize_time: true,
        purge_channels: true,
        temp_roles: false,
        calendar_auto_sync: true,
        role_event: None,
        role_edit: None,
        role_delete: None,
        role_channel: None,
        role_signup: None,
        post_lead_hours: 48,
        digest: "off".into(),
    }
}

#[derive(Insertable)]
#[diesel(table_name = schema::settings)]
struct NewSettings {
    guild_id: i64,
}

pub fn insert_default_settings(conn: &mut Connection, guild_id: i64) -> Result<(), OpError> {
    diesel::insert_into(schema::settings::table)
        .values(&NewSettings { guild_id })
        .execute(conn)?;
    Ok(())
}

pub fn settings(conn: &mut Connection, guild_id: i64) -> Result<Settings, OpError> {
    Settings::query()
        .filter(schema::settings::guild_id.eq(guild_id))
        .first(conn)
        .optional()?
        .ok_or_else(|| OpError::message("This server has no settings yet."))
}

pub fn update_settings(conn: &mut Connection, settings: &Settings) -> Result<(), OpError> {
    diesel::update(settings).set(settings).execute(conn)?;
    Ok(())
}

pub fn set_guild_timezone(conn: &mut Connection, guild_id: i64, timezone: &str) -> Result<(), OpError> {
    diesel::update(schema::guilds::table.filter(schema::guilds::id.eq(guild_id)))
        .set(schema::guilds::timezone.eq(timezone))
        .execute(conn)?;
    Ok(())
}

pub fn list_roles(conn: &mut Connection, guild_id: i64) -> Result<Vec<Role>, OpError> {
    Ok(Role::in_guild(guild_id)
        .order(schema::roles::id.asc())
        .load(conn)?)
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

pub fn list_channels(conn: &mut Connection, guild_id: i64) -> Result<Vec<Channel>, OpError> {
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
    diesel::delete(schema::membership_roles::table.filter(schema::membership_roles::membership_id.eq(membership_id)))
        .execute(conn)?;
    insert_membership_roles(conn, membership_id, &role_ids)
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
    Ok(SignupOption::for_event(event.id).load(conn)?)
}

pub fn options_by_event(
    conn: &mut Connection,
    events: &[Event],
) -> Result<Vec<Vec<SignupOption>>, OpError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = events.iter().map(|event| event.id).collect();
    let rows = SignupOption::for_events(&ids).load(conn)?;
    Ok(rows.grouped_by(events))
}

pub fn option(conn: &mut Connection, id: i64) -> Result<Option<SignupOption>, OpError> {
    Ok(SignupOption::by_id(id).first(conn).optional()?)
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

pub fn list_activity(
    conn: &mut Connection,
    guild_id: i64,
    limit: i64,
) -> Result<Vec<Activity>, OpError> {
    Ok(Activity::in_guild(guild_id)
        .order(schema::activity::id.desc())
        .limit(limit)
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

pub fn thread_messages(
    conn: &mut Connection,
    event: &Event,
) -> Result<Vec<ThreadMessage>, OpError> {
    Ok(ThreadMessage::belonging_to(event)
        .order(schema::thread_messages::id.asc())
        .select(ThreadMessage::as_select())
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

pub fn set_thread_discord_id(
    conn: &mut Connection,
    event_id: i64,
    discord_id: &str,
) -> Result<(), OpError> {
    diesel::update(schema::threads::table.filter(schema::threads::event_id.eq(event_id)))
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
}

#[derive(Insertable)]
#[diesel(table_name = schema::web_sessions)]
struct NewWebSession<'a> {
    token: &'a str,
    discord_user_id: &'a str,
    display_name: &'a str,
    guild_id: i64,
    membership_id: i64,
    created_at: DateTime<Utc>,
}

pub fn insert_web_session(
    conn: &mut Connection,
    token: &str,
    discord_user_id: &str,
    display_name: &str,
    guild_id: i64,
    membership_id: i64,
    guilds: &[i64],
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
    Ok(Some(WebSession {
        token: row.token,
        discord_user_id: row.discord_user_id,
        display_name: row.display_name,
        guild_id: row.guild_id,
        membership_id: row.membership_id,
        created_at: row.created_at,
        guilds,
    }))
}

pub fn move_web_session(
    conn: &mut Connection,
    token: &str,
    guild_id: i64,
    membership_id: i64,
) -> Result<(), OpError> {
    diesel::update(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .set((
            schema::web_sessions::guild_id.eq(guild_id),
            schema::web_sessions::membership_id.eq(membership_id),
        ))
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
        replace_discord_roles(&mut conn, 1, 1, &[]).unwrap();
        let person = membership(&mut conn, 1).unwrap().unwrap();
        assert!(membership_role_ids(&mut conn, &person).unwrap().is_empty());
    }
}
