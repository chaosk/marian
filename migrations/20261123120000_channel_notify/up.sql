-- Who a new event mentions when it's posted, before its creator changes it:
-- the server's default, and a channel's own, which replaces it. A channel
-- with neither column set follows the server; one set to no one and no roles
-- mentions nobody, whatever the server's default.
ALTER TABLE settings
  ADD COLUMN mention_everyone BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN mention_role_ids BIGINT[] NOT NULL DEFAULT '{}';

ALTER TABLE channels
  ADD COLUMN mention_everyone BOOLEAN,
  ADD COLUMN mention_role_ids BIGINT[],
  ADD CONSTRAINT channels_mention_whole
    CHECK ((mention_everyone IS NULL) = (mention_role_ids IS NULL));
