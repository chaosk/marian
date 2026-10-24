-- An event's page lists its own activity, newest first.
CREATE INDEX idx_activity_event ON activity (event_id, id);
