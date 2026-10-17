ALTER TABLE google_oauth ADD COLUMN discord_user_id TEXT;

UPDATE google_oauth AS oauth
SET discord_user_id = users.discord_id
FROM users
WHERE oauth.user_id = users.id;

ALTER TABLE google_oauth ALTER COLUMN discord_user_id SET NOT NULL;
ALTER TABLE google_oauth DROP COLUMN user_id;

ALTER TABLE google_events DROP CONSTRAINT google_events_pkey;
ALTER TABLE google_events ADD COLUMN discord_user_id TEXT;

UPDATE google_events AS link
SET discord_user_id = users.discord_id
FROM users
WHERE link.user_id = users.id;

ALTER TABLE google_events ALTER COLUMN discord_user_id SET NOT NULL;
ALTER TABLE google_events DROP COLUMN user_id;
ALTER TABLE google_events ADD PRIMARY KEY (discord_user_id, event_id);

ALTER TABLE google_accounts DROP CONSTRAINT google_accounts_pkey;
ALTER TABLE google_accounts ADD COLUMN discord_user_id TEXT;

UPDATE google_accounts AS account
SET discord_user_id = users.discord_id
FROM users
WHERE account.user_id = users.id;

ALTER TABLE google_accounts ALTER COLUMN discord_user_id SET NOT NULL;
ALTER TABLE google_accounts DROP COLUMN user_id;
ALTER TABLE google_accounts ADD PRIMARY KEY (discord_user_id);

ALTER TABLE calendar_feeds ADD COLUMN discord_user_id TEXT;

UPDATE calendar_feeds AS feed
SET discord_user_id = users.discord_id
FROM users
WHERE feed.user_id = users.id;

ALTER TABLE calendar_feeds ALTER COLUMN discord_user_id SET NOT NULL;
ALTER TABLE calendar_feeds DROP COLUMN user_id;
ALTER TABLE calendar_feeds ADD CONSTRAINT calendar_feeds_discord_user_id_key UNIQUE (discord_user_id);
