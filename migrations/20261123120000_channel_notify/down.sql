ALTER TABLE channels
  DROP CONSTRAINT channels_mention_whole,
  DROP COLUMN mention_role_ids,
  DROP COLUMN mention_everyone;

ALTER TABLE settings
  DROP COLUMN mention_role_ids,
  DROP COLUMN mention_everyone;
