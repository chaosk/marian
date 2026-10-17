ALTER TABLE channels
  ALTER COLUMN purged_at TYPE text USING to_char(purged_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE event_templates
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE series
  ALTER COLUMN end_on TYPE text USING to_char(end_on AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN anchor_start TYPE text USING to_char(anchor_start AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE events
  ALTER COLUMN starts_at TYPE text USING to_char(starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN post_at TYPE text USING to_char(post_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN posted_at TYPE text USING to_char(posted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN archived_at TYPE text USING to_char(archived_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN cutoff_at TYPE text USING to_char(cutoff_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN updated_at TYPE text USING to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN calendar_synced_at TYPE text USING to_char(calendar_synced_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE signups
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE reminders
  ALTER COLUMN sent_at TYPE text USING to_char(sent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE thread_messages
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE activity
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE command_log
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE web_sessions
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE calendar_feeds
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE google_accounts
  ALTER COLUMN connected_at TYPE text USING to_char(connected_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE google_events
  ALTER COLUMN retry_at TYPE text USING to_char(retry_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE google_oauth
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE routines
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE routine_runs
  ALTER COLUMN opens_at TYPE text USING to_char(opens_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN locks_at TYPE text USING to_char(locks_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');

ALTER TABLE routine_votes
  ALTER COLUMN created_at TYPE text USING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"');
