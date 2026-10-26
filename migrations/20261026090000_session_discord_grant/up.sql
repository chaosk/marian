-- Keeps the Discord sign-in grant with the session, so Marian can re-read
-- which servers the member belongs to without asking them to sign in again.
ALTER TABLE web_sessions
  ADD COLUMN discord_access TEXT,
  ADD COLUMN discord_refresh TEXT,
  ADD COLUMN discord_expires_at TIMESTAMPTZ,
  ADD COLUMN guilds_checked_at TIMESTAMPTZ NOT NULL DEFAULT now();
