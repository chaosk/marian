use std::path::Path;

use diesel::connection::Connection as DbConnection;
use diesel::connection::SimpleConnection;
use diesel::migration::MigrationConnection;
use diesel::prelude::*;
use diesel::sql_query;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

use crate::clock;
use crate::error::OpError;
use crate::schema;

pub use diesel::sqlite::SqliteConnection as Connection;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::guilds)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Guild {
    pub id: i64,
    pub name: String,
    pub timezone: String,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::settings)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
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
    pub park_clock: bool,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct User {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub handle: String,
    pub color: String,
    pub reminders_enabled: bool,
    pub organizer: bool,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::roles)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Role {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub color: String,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::channels)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Channel {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub kind: String,
    pub position: i64,
    pub purged_at: Option<String>,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Event {
    pub id: i64,
    pub public_id: String,
    pub guild_id: i64,
    pub channel_id: i64,
    pub series_id: Option<i64>,
    pub occurrence_index: Option<i64>,
    pub title: String,
    pub description: String,
    pub starts_at: String,
    pub duration_minutes: i64,
    pub status: String,
    pub post_at: Option<String>,
    pub posted_at: Option<String>,
    pub archived_at: Option<String>,
    pub color: String,
    pub image_url: Option<String>,
    pub cutoff_at: Option<String>,
    pub mention_everyone: bool,
    pub attendee_role_id: Option<i64>,
    pub thread_enabled: bool,
    pub multiple_signups: bool,
    pub created_by: i64,
    pub created_at: String,
    pub updated_at: String,
    pub calendar_synced_at: Option<String>,
    pub signups_closed: bool,
    pub post_attempts: i64,
    pub discord_message_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::series)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Series {
    pub id: i64,
    pub public_id: String,
    pub guild_id: i64,
    pub cadence: String,
    pub end_on: Option<String>,
    pub max_occurrences: Option<i64>,
    pub active: bool,
    pub template_id: i64,
    pub anchor_start: String,
    pub post_lead_hours: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::signup_options)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
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

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::signups)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Signup {
    pub id: i64,
    pub event_id: i64,
    pub option_id: i64,
    pub user_id: i64,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::reminders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Reminder {
    pub id: i64,
    pub event_id: i64,
    pub offset_minutes: i64,
    pub message: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::activity)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Activity {
    pub id: i64,
    pub guild_id: i64,
    pub event_id: Option<i64>,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::presets)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Preset {
    pub id: i64,
    pub public_id: String,
    pub guild_id: i64,
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::option_library)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct LibraryOption {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub emoji: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::thread_messages)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ThreadMessage {
    pub id: i64,
    pub event_id: i64,
    pub body: String,
    pub place: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = crate::schema::command_log)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct CommandRow {
    pub id: i64,
    pub guild_id: i64,
    pub user_id: i64,
    pub line: String,
    pub reply: String,
    pub created_at: String,
}

pub fn open(database_url: &str) -> Result<Connection, OpError> {
    let target = parse_database_url(database_url)?;
    if let Some(directory) = &target.directory {
        std::fs::create_dir_all(directory)
            .map_err(|err| OpError::message(format!("Could not create the data folder: {err}")))?;
    }
    let mut conn = Connection::establish(&target.establish)
        .map_err(|err| OpError::message(format!("Could not open the database: {err}")))?;
    tune(&mut conn, target.file, target.system)?;
    migrate(&mut conn)?;
    Ok(conn)
}

pub fn open_memory() -> Result<Connection, OpError> {
    open("sqlite::memory:")
}

fn tune(conn: &mut Connection, file: bool, system: &'static str) -> Result<(), OpError> {
    conn.batch_execute("PRAGMA foreign_keys = ON")?;
    conn.batch_execute("PRAGMA busy_timeout = 5000")?;
    if file {
        let _ = conn.batch_execute("PRAGMA journal_mode = WAL");
    }
    crate::observe::install_query_trace(conn, system);
    Ok(())
}

#[derive(Debug)]
struct DatabaseUrl {
    establish: String,
    directory: Option<std::path::PathBuf>,
    file: bool,
    system: &'static str,
}

fn parse_database_url(database_url: &str) -> Result<DatabaseUrl, OpError> {
    let database_url = database_url.trim();
    if database_url.is_empty() {
        return Err(OpError::message(
            "Set DATABASE_URL to a database connection string, such as sqlite://data/marian.db.",
        ));
    }
    let database_url = normalize_scheme(database_url);
    if database_url == ":memory:" || database_url == "sqlite::memory:" {
        return Ok(memory_url());
    }
    if let Some(spec) = database_url.strip_prefix("sqlite:") {
        return sqlite_url(spec);
    }
    if let Some(spec) = database_url.strip_prefix("file:") {
        return file_url(&database_url, spec);
    }
    if let Some((scheme, _)) = database_url.split_once("://") {
        if !scheme.is_empty() {
            return Err(OpError::message(format!(
                "Set DATABASE_URL to a sqlite connection string, such as sqlite://data/marian.db. The scheme is {scheme}."
            )));
        }
    }
    file_on_disk(database_url.clone(), &database_url)
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

fn memory_url() -> DatabaseUrl {
    DatabaseUrl {
        establish: ":memory:".into(),
        directory: None,
        file: false,
        system: "sqlite",
    }
}

fn sqlite_url(spec: &str) -> Result<DatabaseUrl, OpError> {
    let (body, query) = split_query(spec);
    if body == ":memory:"
        || body == "//:memory:"
        || body == "///:memory:"
        || query_requests_memory(query)
    {
        return Ok(memory_url());
    }
    let (establish, path) = if let Some(rest) = body.strip_prefix("//") {
        (format!("sqlite://{rest}{query}"), rest.to_string())
    } else if let Some(path) = body.strip_prefix('/') {
        (format!("sqlite:///{path}{query}"), format!("/{path}"))
    } else if body.is_empty() {
        return Err(OpError::message(
            "Set DATABASE_URL to a sqlite connection string, such as sqlite://data/marian.db.",
        ));
    } else {
        (format!("sqlite://{body}{query}"), body.to_string())
    };
    file_on_disk(establish, &path)
}

fn file_url(original: &str, spec: &str) -> Result<DatabaseUrl, OpError> {
    let (body, query) = split_query(spec);
    if body == ":memory:" || query_requests_memory(query) {
        return Ok(memory_url());
    }
    let path = if let Some(rest) = body.strip_prefix("//") {
        match rest.split_once('/') {
            Some((host, path)) if host.is_empty() || host.eq_ignore_ascii_case("localhost") => {
                format!("/{path}")
            }
            Some(_) => body.to_string(),
            None => rest.to_string(),
        }
    } else {
        body.to_string()
    };
    if path.is_empty() {
        return Err(OpError::message(
            "Set DATABASE_URL to a sqlite connection string, such as sqlite://data/marian.db.",
        ));
    }
    file_on_disk(original.to_string(), &path)
}

fn file_on_disk(establish: impl Into<String>, path: &str) -> Result<DatabaseUrl, OpError> {
    if path.is_empty() {
        return Err(OpError::message(
            "Set DATABASE_URL to a sqlite connection string, such as sqlite://data/marian.db.",
        ));
    }
    let directory = Path::new(path).parent().and_then(|parent| {
        if parent.as_os_str().is_empty() {
            None
        } else {
            Some(parent.to_path_buf())
        }
    });
    Ok(DatabaseUrl {
        establish: establish.into(),
        directory,
        file: true,
        system: "sqlite",
    })
}

fn split_query(spec: &str) -> (&str, &str) {
    match spec.split_once('?') {
        Some((body, _)) => (body, &spec[body.len()..]),
        None => (spec, ""),
    }
}

fn query_requests_memory(query: &str) -> bool {
    query.trim_start_matches('?').split('&').any(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        key == "mode" && value == "memory"
    })
}

pub fn database_url() -> String {
    choose_database_url(env_value("DATABASE_URL"), env_value("MARIAN_DB"))
}

fn choose_database_url(database_url: Option<String>, legacy: Option<String>) -> String {
    database_url
        .or(legacy)
        .unwrap_or_else(|| "sqlite://data/marian.db".into())
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn migrate(conn: &mut Connection) -> Result<(), OpError> {
    if table_exists(conn, "meta")? && !table_exists(conn, "__diesel_schema_migrations")? {
        legacy_upgrade(conn)?;
        record_applied_migrations(conn)?;
    }
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|err| OpError::message(format!("Could not update the database: {err}")))?;
    Ok(())
}

fn table_exists(conn: &mut Connection, table: &str) -> Result<bool, OpError> {
    #[derive(QueryableByName)]
    struct CountRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    let count: i64 =
        sql_query("SELECT count(*) AS count FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind::<diesel::sql_types::Text, _>(table)
            .get_result::<CountRow>(conn)?
            .count;
    Ok(count > 0)
}

fn record_applied_migrations(conn: &mut Connection) -> Result<(), OpError> {
    MigrationConnection::setup(conn)?;
    // The legacy upgrade already matches the init migration. Later migrations still run.
    sql_query("INSERT INTO __diesel_schema_migrations (version) VALUES (?)")
        .bind::<diesel::sql_types::Text, _>("20261015131400")
        .execute(conn)?;
    Ok(())
}

fn legacy_upgrade(conn: &mut Connection) -> Result<(), OpError> {
    if has_column(conn, "settings", "premium")? {
        sql_query("ALTER TABLE settings DROP COLUMN premium").execute(conn)?;
    }
    if !has_column(conn, "roles", "discord_id")? {
        sql_query("ALTER TABLE roles ADD COLUMN discord_id TEXT").execute(conn)?;
    }
    if !has_column(conn, "threads", "discord_id")? {
        sql_query("ALTER TABLE threads ADD COLUMN discord_id TEXT").execute(conn)?;
    }
    conn.batch_execute(
        "CREATE TABLE IF NOT EXISTS applied_access (
            event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            discord_user_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            PRIMARY KEY (event_id, discord_user_id, kind)
        );
        CREATE TABLE IF NOT EXISTS event_prompts (
            discord_user_id TEXT PRIMARY KEY,
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            step TEXT NOT NULL,
            title TEXT,
            start_raw TEXT,
            channel_name TEXT,
            source_channel TEXT,
            source_discord TEXT,
            draft TEXT
        )",
    )?;
    if !has_column(conn, "event_prompts", "source_channel")? {
        sql_query("ALTER TABLE event_prompts ADD COLUMN source_channel TEXT").execute(conn)?;
    }
    if !has_column(conn, "event_prompts", "source_discord")? {
        sql_query("ALTER TABLE event_prompts ADD COLUMN source_discord TEXT").execute(conn)?;
    }
    if !has_column(conn, "event_prompts", "draft")? {
        sql_query("ALTER TABLE event_prompts ADD COLUMN draft TEXT").execute(conn)?;
    }
    if !has_column(conn, "events", "public_id")? {
        sql_query("ALTER TABLE events ADD COLUMN public_id TEXT").execute(conn)?;
    }
    backfill_public_ids(conn)?;
    sql_query("CREATE UNIQUE INDEX IF NOT EXISTS idx_events_public_id ON events (public_id)")
        .execute(conn)?;
    conn.batch_execute(
        "CREATE TABLE IF NOT EXISTS web_sessions (
            token TEXT PRIMARY KEY,
            discord_user_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS web_session_guilds (
            token TEXT NOT NULL REFERENCES web_sessions(token) ON DELETE CASCADE,
            guild_id INTEGER NOT NULL,
            PRIMARY KEY (token, guild_id)
        );
        CREATE TABLE IF NOT EXISTS calendar_feeds (
            token TEXT PRIMARY KEY,
            discord_user_id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL
        )",
    )?;
    conn.batch_execute(
        "CREATE TABLE IF NOT EXISTS event_templates (
            id INTEGER PRIMARY KEY,
            guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
            channel_id INTEGER NOT NULL REFERENCES channels(id),
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            duration_minutes INTEGER NOT NULL,
            color TEXT NOT NULL DEFAULT '#1e3a6e',
            image_url TEXT,
            thread_enabled INTEGER NOT NULL DEFAULT 0,
            multiple_signups INTEGER NOT NULL DEFAULT 0,
            mention_everyone INTEGER NOT NULL DEFAULT 0,
            attendee_role_id INTEGER REFERENCES roles(id) ON DELETE SET NULL,
            cutoff_offset_minutes INTEGER,
            options_json TEXT NOT NULL DEFAULT '[]',
            reminders_json TEXT NOT NULL DEFAULT '[]',
            mention_role_ids TEXT NOT NULL DEFAULT '[]',
            restrict_role_ids TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS routines (
            id INTEGER PRIMARY KEY,
            public_id TEXT NOT NULL,
            guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
            template_id INTEGER NOT NULL REFERENCES event_templates(id),
            prompt TEXT NOT NULL,
            start_minutes INTEGER NOT NULL,
            poll_weekday INTEGER NOT NULL,
            poll_minutes INTEGER NOT NULL,
            lock_weekday INTEGER NOT NULL,
            lock_minutes INTEGER NOT NULL,
            days_json TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            created_by INTEGER NOT NULL REFERENCES users(id),
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS routine_runs (
            id INTEGER PRIMARY KEY,
            routine_id INTEGER NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
            opens_at TEXT NOT NULL,
            locks_at TEXT NOT NULL,
            status TEXT NOT NULL,
            days_json TEXT NOT NULL,
            winner_weekday INTEGER,
            event_id INTEGER REFERENCES events(id) ON DELETE SET NULL,
            discord_message_id TEXT,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_routine_runs_window ON routine_runs (routine_id, opens_at);
        CREATE TABLE IF NOT EXISTS routine_votes (
            id INTEGER PRIMARY KEY,
            run_id INTEGER NOT NULL REFERENCES routine_runs(id) ON DELETE CASCADE,
            choice TEXT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TEXT NOT NULL,
            UNIQUE (run_id, user_id, choice)
        )",
    )?;
    if !has_column(conn, "routines", "description")? {
        sql_query("ALTER TABLE routines ADD COLUMN description TEXT NOT NULL DEFAULT ''")
            .execute(conn)?;
    }
    if !has_column(conn, "routines", "public_id")? {
        sql_query("ALTER TABLE routines ADD COLUMN public_id TEXT").execute(conn)?;
    }
    if !has_column(conn, "series", "template_id")? {
        sql_query(
            "ALTER TABLE series ADD COLUMN template_id INTEGER REFERENCES event_templates(id)",
        )
        .execute(conn)?;
    }
    if !has_column(conn, "routines", "template_id")? {
        sql_query(
            "ALTER TABLE routines ADD COLUMN template_id INTEGER REFERENCES event_templates(id)",
        )
        .execute(conn)?;
    }
    backfill_event_templates(conn)?;
    backfill_routine_ids(conn)?;
    if !has_column(conn, "series", "public_id")? {
        sql_query("ALTER TABLE series ADD COLUMN public_id TEXT").execute(conn)?;
    }
    if !has_column(conn, "presets", "public_id")? {
        sql_query("ALTER TABLE presets ADD COLUMN public_id TEXT").execute(conn)?;
    }
    backfill_series_ids(conn)?;
    backfill_preset_ids(conn)?;
    sql_query("UPDATE events SET status = 'scheduled' WHERE status = 'ballot'").execute(conn)?;
    sql_query("CREATE UNIQUE INDEX IF NOT EXISTS idx_series_public_id ON series (public_id)")
        .execute(conn)?;
    sql_query("CREATE UNIQUE INDEX IF NOT EXISTS idx_presets_public_id ON presets (public_id)")
        .execute(conn)?;
    sql_query("CREATE UNIQUE INDEX IF NOT EXISTS idx_routines_public_id ON routines (public_id)")
        .execute(conn)?;
    Ok(())
}

fn backfill_event_templates(conn: &mut Connection) -> Result<(), OpError> {
    if has_column(conn, "series", "template_json")? && has_column(conn, "series", "template_id")? {
        let rows =
            sql_query("SELECT id, guild_id, template_json FROM series WHERE template_id IS NULL")
                .load::<OldSeries>(conn)?;
        for row in rows {
            let value: serde_json::Value =
                serde_json::from_str(&row.template_json).unwrap_or(serde_json::json!({}));
            let template_id = insert_template_value(conn, row.guild_id, &value, None)?;
            diesel::update(schema::series::table.filter(schema::series::id.eq(row.id)))
                .set(schema::series::template_id.eq(template_id))
                .execute(conn)?;
        }
    }
    if has_column(conn, "routines", "title")? && has_column(conn, "routines", "template_id")? {
        let rows = sql_query(
            "SELECT id, guild_id, channel_id, title, description, duration_minutes
             FROM routines WHERE template_id IS NULL",
        )
        .load::<OldRoutine>(conn)?;
        let options = serde_json::json!([
            {"name":"Accepted","emoji":"✅","waitlist":false,"reminders_enabled":true},
            {"name":"Declined","emoji":"❌","waitlist":false,"reminders_enabled":true},
            {"name":"Tentative","emoji":"❔","waitlist":false,"reminders_enabled":true}
        ]);
        for row in rows {
            let value = serde_json::json!({
                "channel_id": row.channel_id,
                "title": row.title,
                "description": row.description,
                "duration_minutes": row.duration_minutes,
                "color": "#1e3a6e",
                "thread_enabled": true,
                "options": options,
                "reminders": [{"offset_minutes": 15, "message": ""}],
            });
            let template_id =
                insert_template_value(conn, row.guild_id, &value, Some(row.channel_id))?;
            diesel::update(schema::routines::table.filter(schema::routines::id.eq(row.id)))
                .set(schema::routines::template_id.eq(template_id))
                .execute(conn)?;
        }
    }
    Ok(())
}

#[derive(QueryableByName)]
struct OldSeries {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    guild_id: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    template_json: String,
}

#[derive(QueryableByName)]
struct OldRoutine {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    guild_id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    channel_id: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    title: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    description: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    duration_minutes: i64,
}

#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    id: i64,
}

#[derive(QueryableByName)]
struct ColName {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

fn insert_template_value(
    conn: &mut Connection,
    guild_id: i64,
    value: &serde_json::Value,
    channel_override: Option<i64>,
) -> Result<i64, OpError> {
    let channel_id = channel_override
        .or_else(|| value.get("channel_id").and_then(|item| item.as_i64()))
        .unwrap_or(0);
    let title = value
        .get("title")
        .and_then(|item| item.as_str())
        .unwrap_or("Event");
    let description = value
        .get("description")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let duration = value
        .get("duration_minutes")
        .and_then(|item| item.as_i64())
        .unwrap_or(60);
    let color = value
        .get("color")
        .and_then(|item| item.as_str())
        .unwrap_or("#1e3a6e");
    let image = value
        .get("image_url")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty());
    let flag = |key: &str| {
        value
            .get(key)
            .and_then(|item| item.as_bool())
            .unwrap_or(false)
    };
    let list = |key: &str| {
        value
            .get(key)
            .cloned()
            .unwrap_or(serde_json::json!([]))
            .to_string()
    };
    let id = diesel::insert_into(schema::event_templates::table)
        .values((
            schema::event_templates::guild_id.eq(guild_id),
            schema::event_templates::channel_id.eq(channel_id),
            schema::event_templates::title.eq(title),
            schema::event_templates::description.eq(description),
            schema::event_templates::duration_minutes.eq(duration),
            schema::event_templates::color.eq(color),
            schema::event_templates::image_url.eq(image),
            schema::event_templates::thread_enabled.eq(flag("thread_enabled")),
            schema::event_templates::multiple_signups.eq(flag("multiple_signups")),
            schema::event_templates::mention_everyone.eq(flag("mention_everyone")),
            schema::event_templates::attendee_role_id
                .eq(value.get("attendee_role_id").and_then(|item| item.as_i64())),
            schema::event_templates::cutoff_offset_minutes.eq(value
                .get("cutoff_offset_minutes")
                .and_then(|item| item.as_i64())),
            schema::event_templates::options_json.eq(list("options")),
            schema::event_templates::reminders_json.eq(list("reminders")),
            schema::event_templates::mention_role_ids.eq(list("mention_role_ids")),
            schema::event_templates::restrict_role_ids.eq(list("restrict_role_ids")),
            schema::event_templates::created_at.eq(now_stamp()),
        ))
        .returning(schema::event_templates::id)
        .get_result(conn)?;
    Ok(id)
}

fn backfill_routine_ids(conn: &mut Connection) -> Result<(), OpError> {
    fill_public_ids(conn, "routines")
}

fn backfill_series_ids(conn: &mut Connection) -> Result<(), OpError> {
    fill_public_ids(conn, "series")
}

fn backfill_preset_ids(conn: &mut Connection) -> Result<(), OpError> {
    fill_public_ids(conn, "presets")
}

fn fill_public_ids(conn: &mut Connection, table: &str) -> Result<(), OpError> {
    if !has_column(conn, table, "public_id")? {
        return Ok(());
    }
    let ids = sql_query(format!(
        "SELECT id FROM {table} WHERE public_id IS NULL OR public_id = ''"
    ))
    .load::<IdRow>(conn)?;
    for row in ids {
        let public_id = if table == "routines" {
            crate::ids::routine_id()
        } else {
            crate::ids::event_id()
        };
        sql_query(format!("UPDATE {table} SET public_id = ? WHERE id = ?"))
            .bind::<diesel::sql_types::Text, _>(public_id)
            .bind::<diesel::sql_types::BigInt, _>(row.id)
            .execute(conn)?;
    }
    Ok(())
}

fn backfill_public_ids(conn: &mut Connection) -> Result<(), OpError> {
    fill_public_ids(conn, "events")
}

fn has_column(conn: &mut Connection, table: &str, column: &str) -> Result<bool, OpError> {
    let rows = sql_query(format!("PRAGMA table_info({table})")).load::<ColName>(conn)?;
    Ok(rows.iter().any(|row| row.name == column))
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

pub fn set_meta(conn: &mut Connection, key: &str, value: &str) -> Result<(), OpError> {
    diesel::insert_into(schema::meta::table)
        .values((schema::meta::key.eq(key), schema::meta::value.eq(value)))
        .on_conflict(schema::meta::key)
        .do_update()
        .set(schema::meta::value.eq(value))
        .execute(conn)?;
    Ok(())
}

pub fn list_guilds(conn: &mut Connection) -> Result<Vec<Guild>, OpError> {
    Ok(schema::guilds::table
        .order(schema::guilds::id.asc())
        .select(Guild::as_select())
        .load(conn)?)
}

pub fn guild(conn: &mut Connection, id: i64) -> Result<Option<Guild>, OpError> {
    Ok(schema::guilds::table
        .filter(schema::guilds::id.eq(id))
        .select(Guild::as_select())
        .first(conn)
        .optional()?)
}

pub fn guild_by_discord(conn: &mut Connection, discord_id: &str) -> Result<Option<Guild>, OpError> {
    Ok(schema::guilds::table
        .filter(schema::guilds::discord_id.eq(discord_id))
        .select(Guild::as_select())
        .first(conn)
        .optional()?)
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
        purge_channels: false,
        temp_roles: false,
        calendar_auto_sync: true,
        role_event: None,
        role_edit: None,
        role_delete: None,
        role_channel: None,
        role_signup: None,
        post_lead_hours: 48,
        digest: "off".into(),
        park_clock: false,
    }
}

pub fn insert_default_settings(conn: &mut Connection, guild_id: i64) -> Result<(), OpError> {
    diesel::insert_into(schema::settings::table)
        .values(schema::settings::guild_id.eq(guild_id))
        .execute(conn)?;
    Ok(())
}

pub fn settings(conn: &mut Connection, guild_id: i64) -> Result<Settings, OpError> {
    schema::settings::table
        .filter(schema::settings::guild_id.eq(guild_id))
        .select(Settings::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| OpError::message("This server has no settings yet."))
}

pub fn update_settings(conn: &mut Connection, settings: &Settings) -> Result<(), OpError> {
    diesel::update(
        schema::settings::table.filter(schema::settings::guild_id.eq(settings.guild_id)),
    )
    .set((
        schema::settings::default_reminder_minutes.eq(settings.default_reminder_minutes),
        schema::settings::thread_reminders.eq(settings.thread_reminders),
        schema::settings::auto_archive.eq(settings.auto_archive),
        schema::settings::auto_archive_hours.eq(settings.auto_archive_hours),
        schema::settings::event_threads.eq(settings.event_threads),
        schema::settings::auto_join_threads.eq(settings.auto_join_threads),
        schema::settings::show_attendees.eq(settings.show_attendees),
        schema::settings::hour_24.eq(settings.hour_24),
        schema::settings::localize_time.eq(settings.localize_time),
        schema::settings::purge_channels.eq(settings.purge_channels),
        schema::settings::temp_roles.eq(settings.temp_roles),
        schema::settings::calendar_auto_sync.eq(settings.calendar_auto_sync),
        schema::settings::role_event.eq(settings.role_event),
        schema::settings::role_edit.eq(settings.role_edit),
        schema::settings::role_delete.eq(settings.role_delete),
        schema::settings::role_channel.eq(settings.role_channel),
        schema::settings::role_signup.eq(settings.role_signup),
        schema::settings::post_lead_hours.eq(settings.post_lead_hours),
        schema::settings::digest.eq(&settings.digest),
        schema::settings::park_clock.eq(settings.park_clock),
    ))
    .execute(conn)?;
    Ok(())
}

pub fn list_roles(conn: &mut Connection, guild_id: i64) -> Result<Vec<Role>, OpError> {
    Ok(schema::roles::table
        .filter(schema::roles::guild_id.eq(guild_id))
        .order(schema::roles::id.asc())
        .select(Role::as_select())
        .load(conn)?)
}

pub fn role(conn: &mut Connection, id: i64) -> Result<Option<Role>, OpError> {
    Ok(schema::roles::table
        .filter(schema::roles::id.eq(id))
        .select(Role::as_select())
        .first(conn)
        .optional()?)
}

pub fn role_by_name(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
) -> Result<Option<Role>, OpError> {
    Ok(schema::roles::table
        .filter(schema::roles::guild_id.eq(guild_id))
        .filter(schema::lower(schema::roles::name).eq(schema::lower(name)))
        .select(Role::as_select())
        .first(conn)
        .optional()?)
}

pub fn list_channels(conn: &mut Connection, guild_id: i64) -> Result<Vec<Channel>, OpError> {
    Ok(schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .order((schema::channels::position.asc(), schema::channels::id.asc()))
        .select(Channel::as_select())
        .load(conn)?)
}

pub fn channel(conn: &mut Connection, id: i64) -> Result<Option<Channel>, OpError> {
    Ok(schema::channels::table
        .filter(schema::channels::id.eq(id))
        .select(Channel::as_select())
        .first(conn)
        .optional()?)
}

pub fn channel_by_name(
    conn: &mut Connection,
    guild_id: i64,
    name: &str,
) -> Result<Option<Channel>, OpError> {
    let name = name.trim_start_matches('#');
    Ok(schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .filter(schema::lower(schema::channels::name).eq(schema::lower(name)))
        .select(Channel::as_select())
        .first(conn)
        .optional()?)
}

pub fn channel_by_discord(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<Channel>, OpError> {
    Ok(schema::channels::table
        .filter(schema::channels::guild_id.eq(guild_id))
        .filter(schema::channels::discord_id.eq(discord_id))
        .select(Channel::as_select())
        .first(conn)
        .optional()?)
}

pub fn list_users(conn: &mut Connection, guild_id: i64) -> Result<Vec<User>, OpError> {
    Ok(schema::users::table
        .filter(schema::users::guild_id.eq(guild_id))
        .order(schema::users::name.asc())
        .select(User::as_select())
        .load(conn)?)
}

pub fn user(conn: &mut Connection, id: i64) -> Result<Option<User>, OpError> {
    Ok(schema::users::table
        .filter(schema::users::id.eq(id))
        .select(User::as_select())
        .first(conn)
        .optional()?)
}

pub fn user_by_handle(
    conn: &mut Connection,
    guild_id: i64,
    handle: &str,
) -> Result<Option<User>, OpError> {
    let handle = handle.trim_start_matches('@');
    Ok(schema::users::table
        .filter(schema::users::guild_id.eq(guild_id))
        .filter(schema::lower(schema::users::handle).eq(schema::lower(handle)))
        .select(User::as_select())
        .first(conn)
        .optional()?)
}

pub fn user_by_discord(
    conn: &mut Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<User>, OpError> {
    Ok(schema::users::table
        .filter(schema::users::guild_id.eq(guild_id))
        .filter(schema::users::discord_id.eq(discord_id))
        .select(User::as_select())
        .first(conn)
        .optional()?)
}

pub fn user_role_ids(conn: &mut Connection, user_id: i64) -> Result<Vec<i64>, OpError> {
    Ok(schema::user_roles::table
        .filter(schema::user_roles::user_id.eq(user_id))
        .select(schema::user_roles::role_id)
        .load(conn)?)
}

pub fn user_has_role(conn: &mut Connection, user_id: i64, role_id: i64) -> Result<bool, OpError> {
    let n: i64 = schema::user_roles::table
        .filter(schema::user_roles::user_id.eq(user_id))
        .filter(schema::user_roles::role_id.eq(role_id))
        .count()
        .get_result(conn)?;
    Ok(n > 0)
}

pub fn replace_discord_roles(
    conn: &mut Connection,
    guild_id: i64,
    user_id: i64,
    discord_role_ids: &[String],
) -> Result<(), OpError> {
    let known = list_roles(conn, guild_id)?;
    let role_ids: Vec<i64> = known
        .into_iter()
        .filter(|role| {
            role.discord_id
                .as_deref()
                .is_some_and(|id| discord_role_ids.iter().any(|wanted| wanted == id))
        })
        .map(|role| role.id)
        .collect();
    diesel::delete(schema::user_roles::table.filter(schema::user_roles::user_id.eq(user_id)))
        .execute(conn)?;
    for role_id in role_ids {
        diesel::insert_into(schema::user_roles::table)
            .values((
                schema::user_roles::user_id.eq(user_id),
                schema::user_roles::role_id.eq(role_id),
            ))
            .execute(conn)?;
    }
    Ok(())
}

pub fn event(conn: &mut Connection, id: i64) -> Result<Option<Event>, OpError> {
    Ok(schema::events::table
        .filter(schema::events::id.eq(id))
        .select(Event::as_select())
        .first(conn)
        .optional()?)
}

pub fn event_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Event>, OpError> {
    Ok(schema::events::table
        .filter(schema::events::public_id.eq(public_id))
        .select(Event::as_select())
        .first(conn)
        .optional()?)
}

pub fn discord_server_id(conn: &mut Connection, guild_id: i64) -> Result<String, OpError> {
    let guild = guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    guild
        .discord_id
        .filter(|id| !id.is_empty())
        .ok_or(OpError::NotFound)
}

pub fn list_events(conn: &mut Connection, guild_id: i64) -> Result<Vec<Event>, OpError> {
    Ok(schema::events::table
        .filter(schema::events::guild_id.eq(guild_id))
        .order((schema::events::starts_at.asc(), schema::events::id.asc()))
        .select(Event::as_select())
        .load(conn)?)
}

pub fn events_in_series(conn: &mut Connection, series_id: i64) -> Result<Vec<Event>, OpError> {
    Ok(schema::events::table
        .filter(schema::events::series_id.eq(series_id))
        .order((
            schema::events::occurrence_index.asc(),
            schema::events::id.asc(),
        ))
        .select(Event::as_select())
        .load(conn)?)
}

pub fn list_series(conn: &mut Connection, guild_id: i64) -> Result<Vec<Series>, OpError> {
    Ok(schema::series::table
        .filter(schema::series::guild_id.eq(guild_id))
        .order(schema::series::id.desc())
        .select(Series::as_select())
        .load(conn)?)
}

pub fn series(conn: &mut Connection, id: i64) -> Result<Option<Series>, OpError> {
    Ok(schema::series::table
        .filter(schema::series::id.eq(id))
        .select(Series::as_select())
        .first(conn)
        .optional()?)
}

pub fn series_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Series>, OpError> {
    Ok(schema::series::table
        .filter(schema::series::public_id.eq(public_id))
        .select(Series::as_select())
        .first(conn)
        .optional()?)
}

pub fn options_for(conn: &mut Connection, event_id: i64) -> Result<Vec<SignupOption>, OpError> {
    Ok(schema::signup_options::table
        .filter(schema::signup_options::event_id.eq(event_id))
        .order((
            schema::signup_options::position.asc(),
            schema::signup_options::id.asc(),
        ))
        .select(SignupOption::as_select())
        .load(conn)?)
}

pub fn option(conn: &mut Connection, id: i64) -> Result<Option<SignupOption>, OpError> {
    Ok(schema::signup_options::table
        .filter(schema::signup_options::id.eq(id))
        .select(SignupOption::as_select())
        .first(conn)
        .optional()?)
}

pub fn signups_for(conn: &mut Connection, event_id: i64) -> Result<Vec<Signup>, OpError> {
    Ok(schema::signups::table
        .filter(schema::signups::event_id.eq(event_id))
        .order((schema::signups::created_at.asc(), schema::signups::id.asc()))
        .select(Signup::as_select())
        .load(conn)?)
}

pub fn reminders_for(conn: &mut Connection, event_id: i64) -> Result<Vec<Reminder>, OpError> {
    Ok(schema::reminders::table
        .filter(schema::reminders::event_id.eq(event_id))
        .order((
            schema::reminders::offset_minutes.desc(),
            schema::reminders::id.asc(),
        ))
        .select(Reminder::as_select())
        .load(conn)?)
}

pub fn reminders_in_guild(conn: &mut Connection, guild_id: i64) -> Result<Vec<Reminder>, OpError> {
    Ok(schema::reminders::table
        .inner_join(schema::events::table)
        .filter(schema::events::guild_id.eq(guild_id))
        .order((
            schema::reminders::event_id.asc(),
            schema::reminders::offset_minutes.desc(),
            schema::reminders::id.asc(),
        ))
        .select(Reminder::as_select())
        .load(conn)?)
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
    event_id: i64,
    kind: &str,
) -> Result<Vec<i64>, OpError> {
    Ok(schema::event_roles::table
        .filter(schema::event_roles::event_id.eq(event_id))
        .filter(schema::event_roles::kind.eq(kind))
        .select(schema::event_roles::role_id)
        .load(conn)?)
}

pub fn list_activity(
    conn: &mut Connection,
    guild_id: i64,
    limit: i64,
) -> Result<Vec<Activity>, OpError> {
    Ok(schema::activity::table
        .filter(schema::activity::guild_id.eq(guild_id))
        .order(schema::activity::id.desc())
        .limit(limit)
        .select(Activity::as_select())
        .load(conn)?)
}

pub fn list_presets(conn: &mut Connection, guild_id: i64) -> Result<Vec<Preset>, OpError> {
    Ok(schema::presets::table
        .filter(schema::presets::guild_id.eq(guild_id))
        .order(schema::presets::name.asc())
        .select(Preset::as_select())
        .load(conn)?)
}

pub fn preset(conn: &mut Connection, id: i64) -> Result<Option<Preset>, OpError> {
    Ok(schema::presets::table
        .filter(schema::presets::id.eq(id))
        .select(Preset::as_select())
        .first(conn)
        .optional()?)
}

pub fn preset_by_public(conn: &mut Connection, public_id: &str) -> Result<Option<Preset>, OpError> {
    Ok(schema::presets::table
        .filter(schema::presets::public_id.eq(public_id))
        .select(Preset::as_select())
        .first(conn)
        .optional()?)
}

pub fn list_library(conn: &mut Connection, guild_id: i64) -> Result<Vec<LibraryOption>, OpError> {
    Ok(schema::option_library::table
        .filter(schema::option_library::guild_id.eq(guild_id))
        .order(schema::option_library::name.asc())
        .select(LibraryOption::as_select())
        .load(conn)?)
}

pub fn thread_messages(
    conn: &mut Connection,
    event_id: i64,
) -> Result<Vec<ThreadMessage>, OpError> {
    Ok(schema::thread_messages::table
        .filter(schema::thread_messages::event_id.eq(event_id))
        .order(schema::thread_messages::id.asc())
        .select(ThreadMessage::as_select())
        .load(conn)?)
}

pub fn thread_member_ids(conn: &mut Connection, event_id: i64) -> Result<Vec<i64>, OpError> {
    Ok(schema::thread_members::table
        .filter(schema::thread_members::event_id.eq(event_id))
        .select(schema::thread_members::user_id)
        .load(conn)?)
}

pub fn grant_user_ids(conn: &mut Connection, event_id: i64) -> Result<Vec<i64>, OpError> {
    Ok(schema::role_grants::table
        .filter(schema::role_grants::event_id.eq(event_id))
        .select(schema::role_grants::user_id)
        .load(conn)?)
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

pub fn discord_ids_for_users(
    conn: &mut Connection,
    user_ids: &[i64],
) -> Result<Vec<String>, OpError> {
    let mut ids = Vec::new();
    for user_id in user_ids {
        let discord_id: Option<Option<String>> = schema::users::table
            .filter(schema::users::id.eq(user_id))
            .select(schema::users::discord_id)
            .first(conn)
            .optional()?;
        if let Some(Some(discord_id)) = discord_id {
            ids.push(discord_id);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn applied_user_ids(
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

pub fn mark_applied(
    conn: &mut Connection,
    event_id: i64,
    kind: &str,
    discord_user_id: &str,
) -> Result<(), OpError> {
    diesel::insert_into(schema::applied_access::table)
        .values((
            schema::applied_access::event_id.eq(event_id),
            schema::applied_access::discord_user_id.eq(discord_user_id),
            schema::applied_access::kind.eq(kind),
        ))
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
    Ok(schema::command_log::table
        .filter(schema::command_log::guild_id.eq(guild_id))
        .order(schema::command_log::id.desc())
        .limit(limit)
        .select(CommandRow::as_select())
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
    pub user_id: i64,
    pub created_at: String,
    pub guilds: Vec<i64>,
}

pub fn insert_web_session(
    conn: &mut Connection,
    token: &str,
    discord_user_id: &str,
    display_name: &str,
    guild_id: i64,
    user_id: i64,
    guilds: &[i64],
) -> Result<(), OpError> {
    let token = token.to_string();
    let discord_user_id = discord_user_id.to_string();
    let display_name = display_name.to_string();
    let guilds = guilds.to_vec();
    conn.transaction(|conn| {
        diesel::insert_into(schema::web_sessions::table)
            .values((
                schema::web_sessions::token.eq(&token),
                schema::web_sessions::discord_user_id.eq(&discord_user_id),
                schema::web_sessions::display_name.eq(&display_name),
                schema::web_sessions::guild_id.eq(guild_id),
                schema::web_sessions::user_id.eq(user_id),
                schema::web_sessions::created_at.eq(now_stamp()),
            ))
            .execute(conn)?;
        for guild in &guilds {
            diesel::insert_into(schema::web_session_guilds::table)
                .values((
                    schema::web_session_guilds::token.eq(&token),
                    schema::web_session_guilds::guild_id.eq(guild),
                ))
                .execute(conn)?;
        }
        Ok(())
    })
}

pub fn web_session(conn: &mut Connection, token: &str) -> Result<Option<WebSession>, OpError> {
    let row: Option<(String, String, String, i64, i64, String)> = schema::web_sessions::table
        .filter(schema::web_sessions::token.eq(token))
        .select((
            schema::web_sessions::token,
            schema::web_sessions::discord_user_id,
            schema::web_sessions::display_name,
            schema::web_sessions::guild_id,
            schema::web_sessions::user_id,
            schema::web_sessions::created_at,
        ))
        .first(conn)
        .optional()?;
    let Some((token, discord_user_id, display_name, guild_id, user_id, created_at)) = row else {
        return Ok(None);
    };
    let guilds = schema::web_session_guilds::table
        .filter(schema::web_session_guilds::token.eq(&token))
        .select(schema::web_session_guilds::guild_id)
        .load(conn)?;
    Ok(Some(WebSession {
        token,
        discord_user_id,
        display_name,
        guild_id,
        user_id,
        created_at,
        guilds,
    }))
}

pub fn move_web_session(
    conn: &mut Connection,
    token: &str,
    guild_id: i64,
    user_id: i64,
) -> Result<(), OpError> {
    diesel::update(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .set((
            schema::web_sessions::guild_id.eq(guild_id),
            schema::web_sessions::user_id.eq(user_id),
        ))
        .execute(conn)?;
    Ok(())
}

pub fn delete_web_session(conn: &mut Connection, token: &str) -> Result<(), OpError> {
    diesel::delete(schema::web_sessions::table.filter(schema::web_sessions::token.eq(token)))
        .execute(conn)?;
    Ok(())
}

pub fn ensure_calendar_token(
    conn: &mut Connection,
    discord_user_id: &str,
) -> Result<String, OpError> {
    let existing: Option<String> = schema::calendar_feeds::table
        .filter(schema::calendar_feeds::discord_user_id.eq(discord_user_id))
        .select(schema::calendar_feeds::token)
        .first(conn)
        .optional()?;
    if let Some(token) = existing {
        return Ok(token);
    }
    let token = crate::auth::random_token()?;
    diesel::insert_into(schema::calendar_feeds::table)
        .values((
            schema::calendar_feeds::token.eq(&token),
            schema::calendar_feeds::discord_user_id.eq(discord_user_id),
            schema::calendar_feeds::created_at.eq(now_stamp()),
        ))
        .execute(conn)?;
    Ok(token)
}

pub fn calendar_discord_id(conn: &mut Connection, token: &str) -> Result<Option<String>, OpError> {
    Ok(schema::calendar_feeds::table
        .filter(schema::calendar_feeds::token.eq(token))
        .select(schema::calendar_feeds::discord_user_id)
        .first(conn)
        .optional()?)
}

pub fn now_stamp() -> String {
    clock::stamp(chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    use diesel::prelude::{QueryableByName, RunQueryDsl};

    use super::*;

    #[test]
    fn a_connection_string_names_the_database() {
        let url = parse_database_url("sqlite://data/marian.db").unwrap();
        assert_eq!(url.establish, "sqlite://data/marian.db");
        assert_eq!(url.directory.as_deref(), Some(Path::new("data")));
        assert!(url.file);
        assert_eq!(url.system, "sqlite");

        let url = parse_database_url("sqlite:///data/marian.db?mode=rwc").unwrap();
        assert_eq!(url.establish, "sqlite:///data/marian.db?mode=rwc");
        assert_eq!(url.directory.as_deref(), Some(Path::new("/data")));

        let url = parse_database_url("  SQLITE::memory: ").unwrap();
        assert_eq!(url.establish, ":memory:");
        assert!(!url.file);

        let url = parse_database_url("file:///tmp/marian.db").unwrap();
        assert_eq!(url.establish, "file:///tmp/marian.db");
        assert_eq!(url.directory.as_deref(), Some(Path::new("/tmp")));

        let url = parse_database_url("/data/marian.db").unwrap();
        assert_eq!(url.establish, "/data/marian.db");

        let err = parse_database_url("postgres://user:secret@localhost/marian").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("postgres"));
        assert!(!text.contains("secret"));

        assert_eq!(
            choose_database_url(Some("sqlite://chosen.db".into()), Some("legacy.db".into())),
            "sqlite://chosen.db"
        );
        assert_eq!(
            choose_database_url(None, Some("legacy.db".into())),
            "legacy.db"
        );
        assert_eq!(choose_database_url(None, None), "sqlite://data/marian.db");
    }

    #[test]
    fn a_sqlite_url_opens_that_file() {
        let dir = std::env::temp_dir().join(format!("marian-url-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("marian.db");
        let url = format!("sqlite://{}", path.display());
        let mut conn = open(&url).unwrap();
        assert!(path.is_file());
        assert!(list_guilds(&mut conn).unwrap().is_empty());
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_database_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("marian-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("marian.db");
        let url = format!("sqlite://{}", path.display());
        {
            let mut conn = Connection::establish(&url).unwrap();
            conn.batch_execute(include_str!("../migrations/20261015131400_init/up.sql"))
                .unwrap();
            diesel::insert_into(schema::guilds::table)
                .values((
                    schema::guilds::id.eq(1),
                    schema::guilds::name.eq("Guild"),
                    schema::guilds::timezone.eq("UTC"),
                ))
                .execute(&mut conn)
                .unwrap();
        }
        let mut conn = open(&url).unwrap();
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
            ]
        );
        assert!(table_exists(&mut conn, "google_accounts").unwrap());
        drop(conn);
        let mut conn = open(&url).unwrap();
        assert_eq!(list_guilds(&mut conn).unwrap().len(), 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discord_role_list_replaces_the_stored_roles() {
        let mut conn = open_memory().unwrap();
        conn.batch_execute(
            "
            INSERT INTO guilds (id, name, timezone) VALUES (1, 'Guild', 'UTC');
            INSERT INTO users (id, guild_id, name, handle, color) VALUES
              (1, 1, 'Nell', 'nell', '#112233');
            INSERT INTO roles (id, guild_id, name, color, discord_id) VALUES
              (1, 1, 'Raiders', '#112233', '111'),
              (2, 1, 'Officers', '#112233', '222'),
              (3, 1, 'Local', '#112233', NULL);
            INSERT INTO user_roles (user_id, role_id) VALUES (1, 1), (1, 3);
            ",
        )
        .unwrap();
        replace_discord_roles(&mut conn, 1, 1, &["222".into()]).unwrap();
        let roles = user_role_ids(&mut conn, 1).unwrap();
        assert_eq!(roles, vec![2]);
        replace_discord_roles(&mut conn, 1, 1, &[]).unwrap();
        assert!(user_role_ids(&mut conn, 1).unwrap().is_empty());
    }
}
