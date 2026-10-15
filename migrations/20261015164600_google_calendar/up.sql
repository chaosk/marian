CREATE TABLE google_accounts (
    discord_user_id TEXT PRIMARY KEY,
    refresh_token TEXT NOT NULL,
    email TEXT NOT NULL DEFAULT '',
    connected_at TEXT NOT NULL,
    last_error TEXT NOT NULL DEFAULT ''
);

CREATE TABLE google_events (
    discord_user_id TEXT NOT NULL,
    event_id INTEGER NOT NULL,
    google_event_id TEXT,
    content_hash TEXT NOT NULL DEFAULT '',
    last_error TEXT NOT NULL DEFAULT '',
    retry_at TEXT,
    PRIMARY KEY (discord_user_id, event_id)
);
