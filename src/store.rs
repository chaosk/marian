use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::clock;
use crate::error::OpError;

const SCHEMA: &str = include_str!("../migrations/001_init.sql");

#[derive(Debug, Clone)]
pub struct Guild {
    pub id: i64,
    pub name: String,
    pub timezone: String,
    pub discord_id: Option<String>,
}

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
    pub role_event: Option<i64>,
    pub role_edit: Option<i64>,
    pub role_delete: Option<i64>,
    pub role_channel: Option<i64>,
    pub role_signup: Option<i64>,
    pub post_lead_hours: i64,
    pub digest: String,
    pub park_clock: bool,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct Role {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub color: String,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub kind: String,
    pub position: i64,
    pub purged_at: Option<String>,
    pub discord_id: Option<String>,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct Series {
    pub id: i64,
    pub guild_id: i64,
    pub cadence: String,
    pub end_on: Option<String>,
    pub max_occurrences: Option<i64>,
    pub active: bool,
    pub template_json: String,
    pub anchor_start: String,
    pub post_lead_hours: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct Signup {
    pub id: i64,
    pub event_id: i64,
    pub option_id: i64,
    pub user_id: i64,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct Reminder {
    pub id: i64,
    pub event_id: i64,
    pub offset_minutes: i64,
    pub message: String,
    pub sent_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Activity {
    pub id: i64,
    pub guild_id: i64,
    pub event_id: Option<i64>,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct LibraryOption {
    pub id: i64,
    pub guild_id: i64,
    pub name: String,
    pub emoji: String,
}

#[derive(Debug, Clone)]
pub struct ThreadMessage {
    pub id: i64,
    pub event_id: i64,
    pub body: String,
    pub place: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CommandRow {
    pub id: i64,
    pub guild_id: i64,
    pub user_id: i64,
    pub line: String,
    pub reply: String,
    pub created_at: String,
}

pub fn open(path: &Path) -> Result<Connection, OpError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                OpError::message(format!("Could not create the data folder: {err}"))
            })?;
        }
    }
    let conn = Connection::open(path)?;
    tune(&conn, true)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> Result<Connection, OpError> {
    let conn = Connection::open_in_memory()?;
    tune(&conn, false)?;
    migrate(&conn)?;
    Ok(conn)
}

fn tune(conn: &Connection, file: bool) -> Result<(), OpError> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    if file {
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
    }
    Ok(())
}

pub fn migrate(conn: &Connection) -> Result<(), OpError> {
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
        [],
        |row| row.get(0),
    )?;
    if exists == 0 {
        conn.execute_batch(SCHEMA)?;
    }
    if has_column(conn, "settings", "premium")? {
        conn.execute("ALTER TABLE settings DROP COLUMN premium", [])?;
    }
    if !has_column(conn, "roles", "discord_id")? {
        conn.execute("ALTER TABLE roles ADD COLUMN discord_id TEXT", [])?;
    }
    if !has_column(conn, "threads", "discord_id")? {
        conn.execute("ALTER TABLE threads ADD COLUMN discord_id TEXT", [])?;
    }
    conn.execute_batch(
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
        conn.execute(
            "ALTER TABLE event_prompts ADD COLUMN source_channel TEXT",
            [],
        )?;
    }
    if !has_column(conn, "event_prompts", "source_discord")? {
        conn.execute(
            "ALTER TABLE event_prompts ADD COLUMN source_discord TEXT",
            [],
        )?;
    }
    if !has_column(conn, "event_prompts", "draft")? {
        conn.execute("ALTER TABLE event_prompts ADD COLUMN draft TEXT", [])?;
    }
    if !has_column(conn, "events", "public_id")? {
        conn.execute("ALTER TABLE events ADD COLUMN public_id TEXT", [])?;
    }
    backfill_public_ids(conn)?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_events_public_id ON events (public_id)",
        [],
    )?;
    conn.execute_batch(
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
        );
        CREATE TABLE IF NOT EXISTS rounds (
            id INTEGER PRIMARY KEY,
            series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
            closes_at TEXT NOT NULL,
            status TEXT NOT NULL,
            event_id INTEGER REFERENCES events(id) ON DELETE SET NULL,
            discord_message_id TEXT,
            pins TEXT NOT NULL DEFAULT '{}',
            spec_json TEXT,
            questions_message_id TEXT,
            thread_discord_id TEXT,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_rounds_open ON rounds (series_id) WHERE status = 'open';
        CREATE TABLE IF NOT EXISTS round_votes (
            id INTEGER PRIMARY KEY,
            round_id INTEGER NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
            poll_key TEXT NOT NULL,
            choice TEXT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TEXT NOT NULL,
            UNIQUE (round_id, poll_key, user_id, choice)
        )",
    )?;
    allow_several_day_votes(conn)?;
    if !has_column(conn, "rounds", "spec_json")? {
        conn.execute("ALTER TABLE rounds ADD COLUMN spec_json TEXT", [])?;
    }
    if !has_column(conn, "rounds", "questions_message_id")? {
        conn.execute(
            "ALTER TABLE rounds ADD COLUMN questions_message_id TEXT",
            [],
        )?;
    }
    if !has_column(conn, "rounds", "thread_discord_id")? {
        conn.execute("ALTER TABLE rounds ADD COLUMN thread_discord_id TEXT", [])?;
    }
    Ok(())
}

fn backfill_public_ids(conn: &Connection) -> Result<(), OpError> {
    let mut stmt =
        conn.prepare("SELECT id FROM events WHERE public_id IS NULL OR public_id = ''")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for id in ids {
        conn.execute(
            "UPDATE events SET public_id = ? WHERE id = ?",
            params![crate::ids::event_id(), id],
        )?;
    }
    Ok(())
}

fn allow_several_day_votes(conn: &Connection) -> Result<(), OpError> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'round_votes'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(());
    };
    if sql.contains("user_id, choice)") || sql.contains("user_id,choice)") {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE round_votes_next (
            id INTEGER PRIMARY KEY,
            round_id INTEGER NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
            poll_key TEXT NOT NULL,
            choice TEXT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            created_at TEXT NOT NULL,
            UNIQUE (round_id, poll_key, user_id, choice)
        );
        INSERT INTO round_votes_next (id, round_id, poll_key, choice, user_id, created_at)
            SELECT id, round_id, poll_key, choice, user_id, created_at FROM round_votes;
        DROP TABLE round_votes;
        ALTER TABLE round_votes_next RENAME TO round_votes;",
    )?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, OpError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn with_tx<T>(
    conn: &mut Connection,
    body: impl FnOnce(&Connection) -> Result<T, OpError>,
) -> Result<T, OpError> {
    let tx = conn.transaction()?;
    let value = body(&tx)?;
    tx.commit()?;
    Ok(value)
}

fn flag(row: &Row<'_>, name: &str) -> rusqlite::Result<bool> {
    Ok(row.get::<_, i64>(name)? != 0)
}

pub fn meta(conn: &Connection, key: &str) -> Result<Option<String>, OpError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), OpError> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn list_guilds(conn: &Connection) -> Result<Vec<Guild>, OpError> {
    let mut stmt = conn.prepare("SELECT * FROM guilds ORDER BY id")?;
    let rows = stmt.query_map([], map_guild)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn guild(conn: &Connection, id: i64) -> Result<Option<Guild>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM guilds WHERE id = ?", [id], map_guild)
        .optional()?)
}

pub fn guild_by_discord(conn: &Connection, discord_id: &str) -> Result<Option<Guild>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM guilds WHERE discord_id = ?",
            [discord_id],
            map_guild,
        )
        .optional()?)
}

fn map_guild(row: &Row<'_>) -> rusqlite::Result<Guild> {
    Ok(Guild {
        id: row.get("id")?,
        name: row.get("name")?,
        timezone: row.get("timezone")?,
        discord_id: row.get("discord_id")?,
    })
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

pub fn insert_default_settings(conn: &Connection, guild_id: i64) -> Result<(), OpError> {
    conn.execute("INSERT INTO settings (guild_id) VALUES (?)", [guild_id])?;
    Ok(())
}

pub fn settings(conn: &Connection, guild_id: i64) -> Result<Settings, OpError> {
    conn.query_row(
        "SELECT * FROM settings WHERE guild_id = ?",
        [guild_id],
        map_settings,
    )
    .optional()?
    .ok_or_else(|| OpError::message("This server has no settings yet."))
}

fn map_settings(row: &Row<'_>) -> rusqlite::Result<Settings> {
    Ok(Settings {
        guild_id: row.get("guild_id")?,
        default_reminder_minutes: row.get("default_reminder_minutes")?,
        thread_reminders: flag(row, "thread_reminders")?,
        auto_archive: flag(row, "auto_archive")?,
        auto_archive_hours: row.get("auto_archive_hours")?,
        event_threads: flag(row, "event_threads")?,
        auto_join_threads: flag(row, "auto_join_threads")?,
        show_attendees: flag(row, "show_attendees")?,
        hour_24: flag(row, "hour_24")?,
        localize_time: flag(row, "localize_time")?,
        purge_channels: flag(row, "purge_channels")?,
        temp_roles: flag(row, "temp_roles")?,
        calendar_auto_sync: flag(row, "calendar_auto_sync")?,
        role_event: row.get("role_event")?,
        role_edit: row.get("role_edit")?,
        role_delete: row.get("role_delete")?,
        role_channel: row.get("role_channel")?,
        role_signup: row.get("role_signup")?,
        post_lead_hours: row.get("post_lead_hours")?,
        digest: row.get("digest")?,
        park_clock: flag(row, "park_clock")?,
    })
}

pub fn update_settings(conn: &Connection, settings: &Settings) -> Result<(), OpError> {
    conn.execute(
        "UPDATE settings SET
            default_reminder_minutes = ?, thread_reminders = ?,
            auto_archive = ?, auto_archive_hours = ?, event_threads = ?,
            auto_join_threads = ?, show_attendees = ?, hour_24 = ?, localize_time = ?,
            purge_channels = ?, temp_roles = ?, calendar_auto_sync = ?,
            role_event = ?, role_edit = ?, role_delete = ?, role_channel = ?, role_signup = ?,
            post_lead_hours = ?, digest = ?, park_clock = ?
         WHERE guild_id = ?",
        params![
            settings.default_reminder_minutes,
            i(settings.thread_reminders),
            i(settings.auto_archive),
            settings.auto_archive_hours,
            i(settings.event_threads),
            i(settings.auto_join_threads),
            i(settings.show_attendees),
            i(settings.hour_24),
            i(settings.localize_time),
            i(settings.purge_channels),
            i(settings.temp_roles),
            i(settings.calendar_auto_sync),
            settings.role_event,
            settings.role_edit,
            settings.role_delete,
            settings.role_channel,
            settings.role_signup,
            settings.post_lead_hours,
            settings.digest,
            i(settings.park_clock),
            settings.guild_id,
        ],
    )?;
    Ok(())
}

pub fn list_roles(conn: &Connection, guild_id: i64) -> Result<Vec<Role>, OpError> {
    collect(
        conn,
        "SELECT * FROM roles WHERE guild_id = ? ORDER BY id",
        [guild_id],
        map_role,
    )
}

pub fn role(conn: &Connection, id: i64) -> Result<Option<Role>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM roles WHERE id = ?", [id], map_role)
        .optional()?)
}

pub fn role_by_name(conn: &Connection, guild_id: i64, name: &str) -> Result<Option<Role>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM roles WHERE guild_id = ? AND lower(name) = lower(?)",
            params![guild_id, name],
            map_role,
        )
        .optional()?)
}

fn map_role(row: &Row<'_>) -> rusqlite::Result<Role> {
    Ok(Role {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        name: row.get("name")?,
        color: row.get("color")?,
        discord_id: row.get("discord_id")?,
    })
}

pub fn list_channels(conn: &Connection, guild_id: i64) -> Result<Vec<Channel>, OpError> {
    collect(
        conn,
        "SELECT * FROM channels WHERE guild_id = ? ORDER BY position, id",
        [guild_id],
        map_channel,
    )
}

pub fn channel(conn: &Connection, id: i64) -> Result<Option<Channel>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM channels WHERE id = ?", [id], map_channel)
        .optional()?)
}

pub fn channel_by_name(
    conn: &Connection,
    guild_id: i64,
    name: &str,
) -> Result<Option<Channel>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM channels WHERE guild_id = ? AND lower(name) = lower(?)",
            params![guild_id, name.trim_start_matches('#')],
            map_channel,
        )
        .optional()?)
}

pub fn channel_by_discord(
    conn: &Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<Channel>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM channels WHERE guild_id = ? AND discord_id = ?",
            params![guild_id, discord_id],
            map_channel,
        )
        .optional()?)
}

fn map_channel(row: &Row<'_>) -> rusqlite::Result<Channel> {
    Ok(Channel {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        position: row.get("position")?,
        purged_at: row.get("purged_at")?,
        discord_id: row.get("discord_id")?,
    })
}

pub fn list_users(conn: &Connection, guild_id: i64) -> Result<Vec<User>, OpError> {
    collect(
        conn,
        "SELECT * FROM users WHERE guild_id = ? ORDER BY name",
        [guild_id],
        map_user,
    )
}

pub fn user(conn: &Connection, id: i64) -> Result<Option<User>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM users WHERE id = ?", [id], map_user)
        .optional()?)
}

pub fn user_by_handle(
    conn: &Connection,
    guild_id: i64,
    handle: &str,
) -> Result<Option<User>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM users WHERE guild_id = ? AND lower(handle) = lower(?)",
            params![guild_id, handle.trim_start_matches('@')],
            map_user,
        )
        .optional()?)
}

pub fn user_by_discord(
    conn: &Connection,
    guild_id: i64,
    discord_id: &str,
) -> Result<Option<User>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM users WHERE guild_id = ? AND discord_id = ?",
            params![guild_id, discord_id],
            map_user,
        )
        .optional()?)
}

fn map_user(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        name: row.get("name")?,
        handle: row.get("handle")?,
        color: row.get("color")?,
        reminders_enabled: flag(row, "reminders_enabled")?,
        organizer: flag(row, "organizer")?,
        discord_id: row.get("discord_id")?,
    })
}

pub fn user_role_ids(conn: &Connection, user_id: i64) -> Result<Vec<i64>, OpError> {
    let mut stmt = conn.prepare("SELECT role_id FROM user_roles WHERE user_id = ?")?;
    let rows = stmt.query_map([user_id], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn user_has_role(conn: &Connection, user_id: i64, role_id: i64) -> Result<bool, OpError> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM user_roles WHERE user_id = ? AND role_id = ?",
        params![user_id, role_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

pub fn event(conn: &Connection, id: i64) -> Result<Option<Event>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM events WHERE id = ?", [id], map_event)
        .optional()?)
}

pub fn event_by_public(conn: &Connection, public_id: &str) -> Result<Option<Event>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM events WHERE public_id = ?",
            [public_id],
            map_event,
        )
        .optional()?)
}

pub fn discord_server_id(conn: &Connection, guild_id: i64) -> Result<String, OpError> {
    let guild = guild(conn, guild_id)?.ok_or(OpError::NotFound)?;
    guild
        .discord_id
        .filter(|id| !id.is_empty())
        .ok_or(OpError::NotFound)
}

pub fn list_events(conn: &Connection, guild_id: i64) -> Result<Vec<Event>, OpError> {
    collect(
        conn,
        "SELECT * FROM events WHERE guild_id = ? ORDER BY starts_at, id",
        [guild_id],
        map_event,
    )
}

pub fn events_in_series(conn: &Connection, series_id: i64) -> Result<Vec<Event>, OpError> {
    collect(
        conn,
        "SELECT * FROM events WHERE series_id = ? ORDER BY occurrence_index, id",
        [series_id],
        map_event,
    )
}

fn map_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get("id")?,
        public_id: row.get("public_id")?,
        guild_id: row.get("guild_id")?,
        channel_id: row.get("channel_id")?,
        series_id: row.get("series_id")?,
        occurrence_index: row.get("occurrence_index")?,
        title: row.get("title")?,
        description: row.get("description")?,
        starts_at: row.get("starts_at")?,
        duration_minutes: row.get("duration_minutes")?,
        status: row.get("status")?,
        post_at: row.get("post_at")?,
        posted_at: row.get("posted_at")?,
        archived_at: row.get("archived_at")?,
        color: row.get("color")?,
        image_url: row.get("image_url")?,
        cutoff_at: row.get("cutoff_at")?,
        mention_everyone: flag(row, "mention_everyone")?,
        attendee_role_id: row.get("attendee_role_id")?,
        thread_enabled: flag(row, "thread_enabled")?,
        multiple_signups: flag(row, "multiple_signups")?,
        created_by: row.get("created_by")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        calendar_synced_at: row.get("calendar_synced_at")?,
        signups_closed: flag(row, "signups_closed")?,
        post_attempts: row.get("post_attempts")?,
        discord_message_id: row.get("discord_message_id")?,
    })
}

pub fn list_series(conn: &Connection, guild_id: i64) -> Result<Vec<Series>, OpError> {
    collect(
        conn,
        "SELECT * FROM series WHERE guild_id = ? ORDER BY id DESC",
        [guild_id],
        map_series,
    )
}

pub fn series(conn: &Connection, id: i64) -> Result<Option<Series>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM series WHERE id = ?", [id], map_series)
        .optional()?)
}

fn map_series(row: &Row<'_>) -> rusqlite::Result<Series> {
    Ok(Series {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        cadence: row.get("cadence")?,
        end_on: row.get("end_on")?,
        max_occurrences: row.get("max_occurrences")?,
        active: flag(row, "active")?,
        template_json: row.get("template_json")?,
        anchor_start: row.get("anchor_start")?,
        post_lead_hours: row.get("post_lead_hours")?,
        created_at: row.get("created_at")?,
    })
}

pub fn options_for(conn: &Connection, event_id: i64) -> Result<Vec<SignupOption>, OpError> {
    collect(
        conn,
        "SELECT * FROM signup_options WHERE event_id = ? ORDER BY position, id",
        [event_id],
        map_option,
    )
}

pub fn option(conn: &Connection, id: i64) -> Result<Option<SignupOption>, OpError> {
    Ok(conn
        .query_row(
            "SELECT * FROM signup_options WHERE id = ?",
            [id],
            map_option,
        )
        .optional()?)
}

fn map_option(row: &Row<'_>) -> rusqlite::Result<SignupOption> {
    Ok(SignupOption {
        id: row.get("id")?,
        event_id: row.get("event_id")?,
        name: row.get("name")?,
        emoji: row.get("emoji")?,
        capacity: row.get("capacity")?,
        waitlist: flag(row, "waitlist")?,
        role_id: row.get("role_id")?,
        reminders_enabled: flag(row, "reminders_enabled")?,
        position: row.get("position")?,
    })
}

pub fn signups_for(conn: &Connection, event_id: i64) -> Result<Vec<Signup>, OpError> {
    collect(
        conn,
        "SELECT * FROM signups WHERE event_id = ? ORDER BY created_at, id",
        [event_id],
        map_signup,
    )
}

fn map_signup(row: &Row<'_>) -> rusqlite::Result<Signup> {
    Ok(Signup {
        id: row.get("id")?,
        event_id: row.get("event_id")?,
        option_id: row.get("option_id")?,
        user_id: row.get("user_id")?,
        state: row.get("state")?,
        created_at: row.get("created_at")?,
    })
}

pub fn reminders_for(conn: &Connection, event_id: i64) -> Result<Vec<Reminder>, OpError> {
    collect(
        conn,
        "SELECT * FROM reminders WHERE event_id = ? ORDER BY offset_minutes DESC, id",
        [event_id],
        map_reminder,
    )
}

fn map_reminder(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    Ok(Reminder {
        id: row.get("id")?,
        event_id: row.get("event_id")?,
        offset_minutes: row.get("offset_minutes")?,
        message: row.get("message")?,
        sent_at: row.get("sent_at")?,
    })
}

pub fn event_role_ids(conn: &Connection, event_id: i64, kind: &str) -> Result<Vec<i64>, OpError> {
    let mut stmt =
        conn.prepare("SELECT role_id FROM event_roles WHERE event_id = ? AND kind = ?")?;
    let rows = stmt.query_map(params![event_id, kind], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_activity(
    conn: &Connection,
    guild_id: i64,
    limit: i64,
) -> Result<Vec<Activity>, OpError> {
    let mut stmt =
        conn.prepare("SELECT * FROM activity WHERE guild_id = ? ORDER BY id DESC LIMIT ?")?;
    let rows = stmt.query_map(params![guild_id, limit], map_activity)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_activity(row: &Row<'_>) -> rusqlite::Result<Activity> {
    Ok(Activity {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        event_id: row.get("event_id")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
    })
}

pub fn list_presets(conn: &Connection, guild_id: i64) -> Result<Vec<Preset>, OpError> {
    collect(
        conn,
        "SELECT * FROM presets WHERE guild_id = ? ORDER BY name",
        [guild_id],
        map_preset,
    )
}

pub fn preset(conn: &Connection, id: i64) -> Result<Option<Preset>, OpError> {
    Ok(conn
        .query_row("SELECT * FROM presets WHERE id = ?", [id], map_preset)
        .optional()?)
}

fn map_preset(row: &Row<'_>) -> rusqlite::Result<Preset> {
    Ok(Preset {
        id: row.get("id")?,
        guild_id: row.get("guild_id")?,
        name: row.get("name")?,
        body: row.get("body")?,
    })
}

pub fn list_library(conn: &Connection, guild_id: i64) -> Result<Vec<LibraryOption>, OpError> {
    collect(
        conn,
        "SELECT * FROM option_library WHERE guild_id = ? ORDER BY name",
        [guild_id],
        |row| {
            Ok(LibraryOption {
                id: row.get("id")?,
                guild_id: row.get("guild_id")?,
                name: row.get("name")?,
                emoji: row.get("emoji")?,
            })
        },
    )
}

pub fn thread_messages(conn: &Connection, event_id: i64) -> Result<Vec<ThreadMessage>, OpError> {
    collect(
        conn,
        "SELECT * FROM thread_messages WHERE event_id = ? ORDER BY id",
        [event_id],
        |row| {
            Ok(ThreadMessage {
                id: row.get("id")?,
                event_id: row.get("event_id")?,
                body: row.get("body")?,
                place: row.get("place")?,
                created_at: row.get("created_at")?,
            })
        },
    )
}

pub fn thread_member_ids(conn: &Connection, event_id: i64) -> Result<Vec<i64>, OpError> {
    let mut stmt = conn.prepare("SELECT user_id FROM thread_members WHERE event_id = ?")?;
    let rows = stmt.query_map([event_id], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn grant_user_ids(conn: &Connection, event_id: i64) -> Result<Vec<i64>, OpError> {
    let mut stmt = conn.prepare("SELECT user_id FROM role_grants WHERE event_id = ?")?;
    let rows = stmt.query_map([event_id], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn thread_discord_id(conn: &Connection, event_id: i64) -> Result<Option<String>, OpError> {
    Ok(conn
        .query_row(
            "SELECT discord_id FROM threads WHERE event_id = ?",
            [event_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub fn set_thread_discord_id(
    conn: &Connection,
    event_id: i64,
    discord_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE threads SET discord_id = ? WHERE event_id = ?",
        params![discord_id, event_id],
    )?;
    Ok(())
}

pub fn set_role_discord_id(
    conn: &Connection,
    role_id: i64,
    discord_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE roles SET discord_id = ? WHERE id = ?",
        params![discord_id, role_id],
    )?;
    Ok(())
}

pub fn discord_ids_for_users(conn: &Connection, user_ids: &[i64]) -> Result<Vec<String>, OpError> {
    let mut ids = Vec::new();
    for user_id in user_ids {
        let discord_id: Option<String> = conn
            .query_row(
                "SELECT discord_id FROM users WHERE id = ?",
                [user_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(discord_id) = discord_id {
            ids.push(discord_id);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn applied_user_ids(
    conn: &Connection,
    event_id: i64,
    kind: &str,
) -> Result<Vec<String>, OpError> {
    let mut stmt = conn.prepare(
        "SELECT discord_user_id FROM applied_access WHERE event_id = ? AND kind = ? ORDER BY discord_user_id",
    )?;
    let rows = stmt.query_map(params![event_id, kind], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn mark_applied(
    conn: &Connection,
    event_id: i64,
    kind: &str,
    discord_user_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "INSERT OR IGNORE INTO applied_access (event_id, discord_user_id, kind) VALUES (?, ?, ?)",
        params![event_id, discord_user_id, kind],
    )?;
    Ok(())
}

pub fn clear_applied(
    conn: &Connection,
    event_id: i64,
    kind: &str,
    discord_user_id: &str,
) -> Result<(), OpError> {
    conn.execute(
        "DELETE FROM applied_access WHERE event_id = ? AND kind = ? AND discord_user_id = ?",
        params![event_id, kind, discord_user_id],
    )?;
    Ok(())
}

pub fn list_commands(
    conn: &Connection,
    guild_id: i64,
    limit: i64,
) -> Result<Vec<CommandRow>, OpError> {
    let mut stmt =
        conn.prepare("SELECT * FROM command_log WHERE guild_id = ? ORDER BY id DESC LIMIT ?")?;
    let rows = stmt.query_map(params![guild_id, limit], |row| {
        Ok(CommandRow {
            id: row.get("id")?,
            guild_id: row.get("guild_id")?,
            user_id: row.get("user_id")?,
            line: row.get("line")?,
            reply: row.get("reply")?,
            created_at: row.get("created_at")?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn count_status(conn: &Connection, guild_id: i64, status: &str) -> Result<i64, OpError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM events WHERE guild_id = ? AND status = ?",
        params![guild_id, status],
        |row| row.get(0),
    )?)
}

pub fn count_active_series(conn: &Connection, guild_id: i64) -> Result<i64, OpError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM series WHERE guild_id = ? AND active = 1 AND cadence != 'once'",
        [guild_id],
        |row| row.get(0),
    )?)
}

#[derive(Debug, Clone)]
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
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO web_sessions
            (token, discord_user_id, display_name, guild_id, user_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            token,
            discord_user_id,
            display_name,
            guild_id,
            user_id,
            now_stamp()
        ],
    )?;
    for guild in guilds {
        tx.execute(
            "INSERT INTO web_session_guilds (token, guild_id) VALUES (?, ?)",
            params![token, guild],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn web_session(conn: &Connection, token: &str) -> Result<Option<WebSession>, OpError> {
    let row = conn.query_row(
        "SELECT token, discord_user_id, display_name, guild_id, user_id, created_at
         FROM web_sessions WHERE token = ?",
        [token],
        |row| {
            Ok(WebSession {
                token: row.get(0)?,
                discord_user_id: row.get(1)?,
                display_name: row.get(2)?,
                guild_id: row.get(3)?,
                user_id: row.get(4)?,
                created_at: row.get(5)?,
                guilds: Vec::new(),
            })
        },
    );
    let mut session = match row {
        Ok(session) => session,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut stmt = conn.prepare("SELECT guild_id FROM web_session_guilds WHERE token = ?")?;
    let guilds = stmt.query_map([token], |row| row.get::<_, i64>(0))?;
    session.guilds = guilds.collect::<Result<Vec<_>, _>>()?;
    Ok(Some(session))
}

pub fn move_web_session(
    conn: &Connection,
    token: &str,
    guild_id: i64,
    user_id: i64,
) -> Result<(), OpError> {
    conn.execute(
        "UPDATE web_sessions SET guild_id = ?, user_id = ? WHERE token = ?",
        params![guild_id, user_id, token],
    )?;
    Ok(())
}

pub fn delete_web_session(conn: &Connection, token: &str) -> Result<(), OpError> {
    conn.execute("DELETE FROM web_sessions WHERE token = ?", [token])?;
    Ok(())
}

pub fn ensure_calendar_token(conn: &Connection, discord_user_id: &str) -> Result<String, OpError> {
    if let Some(token) = conn
        .query_row(
            "SELECT token FROM calendar_feeds WHERE discord_user_id = ?",
            [discord_user_id],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(token);
    }
    let token = crate::auth::random_token()?;
    conn.execute(
        "INSERT INTO calendar_feeds (token, discord_user_id, created_at) VALUES (?, ?, ?)",
        params![token, discord_user_id, now_stamp()],
    )?;
    Ok(token)
}

pub fn calendar_discord_id(conn: &Connection, token: &str) -> Result<Option<String>, OpError> {
    Ok(conn
        .query_row(
            "SELECT discord_user_id FROM calendar_feeds WHERE token = ?",
            [token],
            |row| row.get(0),
        )
        .optional()?)
}

fn collect<T, F>(conn: &Connection, sql: &str, id: [i64; 1], map: F) -> Result<Vec<T>, OpError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(id, map)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn i(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

pub fn now_stamp() -> String {
    clock::stamp(chrono::Utc::now())
}
