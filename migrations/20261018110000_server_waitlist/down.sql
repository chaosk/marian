ALTER TABLE signup_options ADD COLUMN waitlist BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE signup_options SET waitlist = TRUE
WHERE capacity IS NOT NULL
  AND event_id IN (
    SELECT events.id FROM events JOIN settings ON settings.guild_id = events.guild_id WHERE settings.waitlist
  );

ALTER TABLE settings DROP COLUMN waitlist;
