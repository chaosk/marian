-- Marian leaves thread membership to the reminders that mention people,
-- unless a server asks it to pull attendees in.
ALTER TABLE settings ALTER COLUMN auto_join_threads SET DEFAULT FALSE;
