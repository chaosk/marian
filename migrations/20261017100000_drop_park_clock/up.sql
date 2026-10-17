DELETE FROM meta WHERE key LIKE 'anchor:%';

ALTER TABLE settings DROP COLUMN park_clock;
