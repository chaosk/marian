ALTER TABLE settings ALTER COLUMN purge_channels SET DEFAULT TRUE;
UPDATE settings SET purge_channels = TRUE;
