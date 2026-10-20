-- A permission can name several roles. Each row lets one role do one thing.
CREATE TABLE permission_roles (
  guild_id BIGINT NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  role_id BIGINT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  PRIMARY KEY (guild_id, kind, role_id)
);

INSERT INTO permission_roles (guild_id, kind, role_id)
SELECT guild_id, 'event', role_event FROM settings WHERE role_event IS NOT NULL
UNION ALL
SELECT guild_id, 'edit', role_edit FROM settings WHERE role_edit IS NOT NULL
UNION ALL
SELECT guild_id, 'delete', role_delete FROM settings WHERE role_delete IS NOT NULL
UNION ALL
SELECT guild_id, 'channel', role_channel FROM settings WHERE role_channel IS NOT NULL
UNION ALL
SELECT guild_id, 'signup', role_signup FROM settings WHERE role_signup IS NOT NULL;

ALTER TABLE settings
  DROP COLUMN role_event,
  DROP COLUMN role_edit,
  DROP COLUMN role_delete,
  DROP COLUMN role_channel,
  DROP COLUMN role_signup;
