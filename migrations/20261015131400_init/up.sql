CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE guilds (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  timezone TEXT NOT NULL,
  discord_id TEXT
);

CREATE TABLE settings (
  guild_id INTEGER PRIMARY KEY REFERENCES guilds(id) ON DELETE CASCADE,
  default_reminder_minutes INTEGER NOT NULL DEFAULT 15,
  thread_reminders INTEGER NOT NULL DEFAULT 1,
  auto_archive INTEGER NOT NULL DEFAULT 1,
  auto_archive_hours INTEGER NOT NULL DEFAULT 12,
  event_threads INTEGER NOT NULL DEFAULT 1,
  auto_join_threads INTEGER NOT NULL DEFAULT 1,
  show_attendees INTEGER NOT NULL DEFAULT 1,
  hour_24 INTEGER NOT NULL DEFAULT 0,
  localize_time INTEGER NOT NULL DEFAULT 1,
  purge_channels INTEGER NOT NULL DEFAULT 0,
  temp_roles INTEGER NOT NULL DEFAULT 0,
  calendar_auto_sync INTEGER NOT NULL DEFAULT 1,
  role_event INTEGER,
  role_edit INTEGER,
  role_delete INTEGER,
  role_channel INTEGER,
  role_signup INTEGER,
  post_lead_hours INTEGER NOT NULL DEFAULT 48,
  digest TEXT NOT NULL DEFAULT 'off',
  park_clock INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE roles (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  color TEXT NOT NULL,
  discord_id TEXT
);

CREATE TABLE channels (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'text',
  position INTEGER NOT NULL DEFAULT 0,
  purged_at TEXT,
  discord_id TEXT
);

CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  handle TEXT NOT NULL,
  color TEXT NOT NULL,
  reminders_enabled INTEGER NOT NULL DEFAULT 1,
  organizer INTEGER NOT NULL DEFAULT 0,
  discord_id TEXT,
  UNIQUE (guild_id, handle)
);

CREATE TABLE user_roles (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role_id INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, role_id)
);

CREATE TABLE event_templates (
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

CREATE TABLE series (
  id INTEGER PRIMARY KEY,
  public_id TEXT NOT NULL,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  cadence TEXT NOT NULL,
  end_on TEXT,
  max_occurrences INTEGER,
  active INTEGER NOT NULL DEFAULT 1,
  template_id INTEGER NOT NULL REFERENCES event_templates(id),
  anchor_start TEXT NOT NULL,
  post_lead_hours INTEGER NOT NULL DEFAULT 48,
  created_at TEXT NOT NULL
);

CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  public_id TEXT NOT NULL,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  channel_id INTEGER NOT NULL REFERENCES channels(id),
  series_id INTEGER REFERENCES series(id) ON DELETE SET NULL,
  occurrence_index INTEGER,
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  starts_at TEXT NOT NULL,
  duration_minutes INTEGER NOT NULL DEFAULT 60,
  status TEXT NOT NULL,
  post_at TEXT,
  posted_at TEXT,
  archived_at TEXT,
  color TEXT NOT NULL DEFAULT '#1e3a6e',
  image_url TEXT,
  cutoff_at TEXT,
  mention_everyone INTEGER NOT NULL DEFAULT 0,
  attendee_role_id INTEGER REFERENCES roles(id) ON DELETE SET NULL,
  thread_enabled INTEGER NOT NULL DEFAULT 0,
  multiple_signups INTEGER NOT NULL DEFAULT 0,
  created_by INTEGER NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  calendar_synced_at TEXT,
  signups_closed INTEGER NOT NULL DEFAULT 0,
  post_attempts INTEGER NOT NULL DEFAULT 0,
  discord_message_id TEXT
);

CREATE TABLE event_roles (
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  role_id INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  PRIMARY KEY (event_id, role_id, kind)
);

CREATE TABLE signup_options (
  id INTEGER PRIMARY KEY,
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  emoji TEXT NOT NULL,
  capacity INTEGER,
  waitlist INTEGER NOT NULL DEFAULT 0,
  role_id INTEGER,
  reminders_enabled INTEGER NOT NULL DEFAULT 1,
  position INTEGER NOT NULL
);

CREATE TABLE signups (
  id INTEGER PRIMARY KEY,
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  option_id INTEGER NOT NULL REFERENCES signup_options(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (option_id, user_id)
);

CREATE TABLE reminders (
  id INTEGER PRIMARY KEY,
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  offset_minutes INTEGER NOT NULL,
  message TEXT NOT NULL DEFAULT '',
  sent_at TEXT
);

CREATE TABLE threads (
  event_id INTEGER PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  discord_id TEXT
);

CREATE TABLE applied_access (
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  discord_user_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  PRIMARY KEY (event_id, discord_user_id, kind)
);

CREATE TABLE thread_members (
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  PRIMARY KEY (event_id, user_id)
);

CREATE TABLE thread_messages (
  id INTEGER PRIMARY KEY,
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  body TEXT NOT NULL,
  place TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE role_grants (
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role_id INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  PRIMARY KEY (event_id, user_id)
);

CREATE TABLE presets (
  id INTEGER PRIMARY KEY,
  public_id TEXT NOT NULL,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  body TEXT NOT NULL
);

CREATE TABLE option_library (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  emoji TEXT NOT NULL,
  UNIQUE (guild_id, name)
);

CREATE TABLE activity (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  event_id INTEGER REFERENCES events(id) ON DELETE SET NULL,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE command_log (
  id INTEGER PRIMARY KEY,
  guild_id INTEGER NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL,
  line TEXT NOT NULL,
  reply TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE event_prompts (
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
);

CREATE TABLE web_sessions (
  token TEXT PRIMARY KEY,
  discord_user_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  guild_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE web_session_guilds (
  token TEXT NOT NULL REFERENCES web_sessions(token) ON DELETE CASCADE,
  guild_id INTEGER NOT NULL,
  PRIMARY KEY (token, guild_id)
);

CREATE TABLE calendar_feeds (
  token TEXT PRIMARY KEY,
  discord_user_id TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL
);

CREATE TABLE routines (
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
  created_at TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT ''
);

CREATE UNIQUE INDEX idx_routines_public_id ON routines (public_id);

CREATE TABLE routine_runs (
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

CREATE UNIQUE INDEX idx_routine_runs_window ON routine_runs (routine_id, opens_at);

CREATE TABLE routine_votes (
  id INTEGER PRIMARY KEY,
  run_id INTEGER NOT NULL REFERENCES routine_runs(id) ON DELETE CASCADE,
  choice TEXT NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  UNIQUE (run_id, user_id, choice)
);

CREATE UNIQUE INDEX idx_events_public_id ON events (public_id);
CREATE UNIQUE INDEX idx_series_public_id ON series (public_id);
CREATE UNIQUE INDEX idx_presets_public_id ON presets (public_id);
CREATE INDEX idx_events_guild_status ON events (guild_id, status, starts_at);
CREATE INDEX idx_events_series ON events (series_id, occurrence_index);
CREATE INDEX idx_signups_event ON signups (event_id, option_id, state);
CREATE INDEX idx_activity_guild ON activity (guild_id, id);
