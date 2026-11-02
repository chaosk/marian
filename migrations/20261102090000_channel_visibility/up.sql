-- What Discord lets each role and channel see, so Marian shows a member only
-- the events in channels they can view. A server whose @everyone permissions
-- aren't known yet shows every channel, as before.
ALTER TABLE guilds ADD COLUMN everyone_permissions BIGINT;
ALTER TABLE roles ADD COLUMN permissions BIGINT NOT NULL DEFAULT 0;

CREATE TABLE channel_overwrites (
  channel_id BIGINT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
  -- 'role' or 'member'
  kind TEXT NOT NULL,
  -- The Discord id of the role (the server's own id for @everyone) or member.
  target TEXT NOT NULL,
  allow BIGINT NOT NULL,
  deny BIGINT NOT NULL,
  PRIMARY KEY (channel_id, kind, target)
);
