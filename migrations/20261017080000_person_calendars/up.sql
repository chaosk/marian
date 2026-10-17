ALTER TABLE calendar_feeds ADD COLUMN user_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

UPDATE calendar_feeds AS feed
SET user_id = users.id
FROM users
WHERE feed.discord_user_id = users.discord_id;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM calendar_feeds WHERE user_id IS NULL) THEN
    RAISE EXCEPTION 'A calendar feed has no user';
  END IF;
END $$;

ALTER TABLE calendar_feeds ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE calendar_feeds DROP COLUMN discord_user_id;
ALTER TABLE calendar_feeds ADD CONSTRAINT calendar_feeds_user_id_key UNIQUE (user_id);

ALTER TABLE google_accounts ADD COLUMN user_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

UPDATE google_accounts AS account
SET user_id = users.id
FROM users
WHERE account.discord_user_id = users.discord_id;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM google_accounts WHERE user_id IS NULL) THEN
    RAISE EXCEPTION 'A Google Calendar connection has no user';
  END IF;
END $$;

ALTER TABLE google_accounts DROP CONSTRAINT google_accounts_pkey;
ALTER TABLE google_accounts DROP COLUMN discord_user_id;
ALTER TABLE google_accounts ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE google_accounts ADD PRIMARY KEY (user_id);

ALTER TABLE google_events ADD COLUMN user_id BIGINT;

UPDATE google_events AS link
SET user_id = users.id
FROM users
WHERE link.discord_user_id = users.discord_id;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM google_events WHERE user_id IS NULL) THEN
    RAISE EXCEPTION 'A Google Calendar event has no user';
  END IF;
END $$;

ALTER TABLE google_events DROP CONSTRAINT google_events_pkey;
ALTER TABLE google_events DROP COLUMN discord_user_id;
ALTER TABLE google_events ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE google_events ADD PRIMARY KEY (user_id, event_id);
ALTER TABLE google_events
  ADD CONSTRAINT google_events_user_id_fkey
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE google_oauth ADD COLUMN user_id BIGINT REFERENCES users(id) ON DELETE CASCADE;

UPDATE google_oauth AS oauth
SET user_id = users.id
FROM users
WHERE oauth.discord_user_id = users.discord_id;

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM google_oauth WHERE user_id IS NULL) THEN
    RAISE EXCEPTION 'A Google sign-in has no user';
  END IF;
END $$;

ALTER TABLE google_oauth ALTER COLUMN user_id SET NOT NULL;
ALTER TABLE google_oauth DROP COLUMN discord_user_id;
