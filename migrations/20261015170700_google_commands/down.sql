DROP TABLE IF EXISTS google_oauth;
ALTER TABLE google_events DROP COLUMN forced;
ALTER TABLE google_accounts DROP COLUMN revoking;
ALTER TABLE google_accounts DROP COLUMN auto_sync;
