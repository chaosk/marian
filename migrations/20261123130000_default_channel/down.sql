DROP INDEX idx_events_created_by;

ALTER TABLE settings
  DROP COLUMN default_channel_id;
