-- The channel a new event starts in for someone who hasn't made one here yet.
-- None starts it in the first channel listed.
ALTER TABLE settings
  ADD COLUMN default_channel_id BIGINT REFERENCES channels(id) ON DELETE SET NULL;

-- A person's newest event, to start their next one in the same channel.
CREATE INDEX idx_events_created_by ON events (created_by, created_at DESC);
