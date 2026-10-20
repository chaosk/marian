-- The waitlist is a server setting instead of an option flag. Servers that used
-- it on any option keep it on.
ALTER TABLE settings ADD COLUMN waitlist BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE settings SET waitlist = TRUE
WHERE guild_id IN (
  SELECT events.guild_id
  FROM signup_options
  JOIN events ON events.id = signup_options.event_id
  WHERE signup_options.waitlist
);

ALTER TABLE signup_options DROP COLUMN waitlist;
