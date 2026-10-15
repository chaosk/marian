ALTER TABLE google_accounts ADD COLUMN auto_sync INTEGER NOT NULL DEFAULT 1;
ALTER TABLE google_accounts ADD COLUMN revoking INTEGER NOT NULL DEFAULT 0;

ALTER TABLE google_events ADD COLUMN forced INTEGER NOT NULL DEFAULT 0;

CREATE TABLE google_oauth (
    state TEXT PRIMARY KEY,
    discord_user_id TEXT NOT NULL,
    created_at TEXT NOT NULL
);
