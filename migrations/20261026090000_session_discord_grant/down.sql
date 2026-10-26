ALTER TABLE web_sessions
  DROP COLUMN guilds_checked_at,
  DROP COLUMN discord_expires_at,
  DROP COLUMN discord_refresh,
  DROP COLUMN discord_access;
