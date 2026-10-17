ALTER TABLE channels
  ALTER COLUMN purged_at TYPE timestamptz USING purged_at::timestamptz;

ALTER TABLE event_templates
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE series
  ALTER COLUMN end_on TYPE timestamptz USING end_on::timestamptz,
  ALTER COLUMN anchor_start TYPE timestamptz USING anchor_start::timestamptz,
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE events
  ALTER COLUMN starts_at TYPE timestamptz USING starts_at::timestamptz,
  ALTER COLUMN post_at TYPE timestamptz USING post_at::timestamptz,
  ALTER COLUMN posted_at TYPE timestamptz USING posted_at::timestamptz,
  ALTER COLUMN archived_at TYPE timestamptz USING archived_at::timestamptz,
  ALTER COLUMN cutoff_at TYPE timestamptz USING cutoff_at::timestamptz,
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz,
  ALTER COLUMN updated_at TYPE timestamptz USING updated_at::timestamptz,
  ALTER COLUMN calendar_synced_at TYPE timestamptz USING calendar_synced_at::timestamptz;

ALTER TABLE signups
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE reminders
  ALTER COLUMN sent_at TYPE timestamptz USING sent_at::timestamptz;

ALTER TABLE thread_messages
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE activity
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE command_log
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE web_sessions
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE calendar_feeds
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE google_accounts
  ALTER COLUMN connected_at TYPE timestamptz USING connected_at::timestamptz;

ALTER TABLE google_events
  ALTER COLUMN retry_at TYPE timestamptz USING retry_at::timestamptz;

ALTER TABLE google_oauth
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE routines
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE routine_runs
  ALTER COLUMN opens_at TYPE timestamptz USING opens_at::timestamptz,
  ALTER COLUMN locks_at TYPE timestamptz USING locks_at::timestamptz,
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;

ALTER TABLE routine_votes
  ALTER COLUMN created_at TYPE timestamptz USING created_at::timestamptz;
