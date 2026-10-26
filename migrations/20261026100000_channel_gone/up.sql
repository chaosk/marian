-- A Discord channel that was deleted keeps its row for the events posted in it,
-- but drops out of the channel lists.
ALTER TABLE channels ADD COLUMN gone_at TIMESTAMPTZ;
