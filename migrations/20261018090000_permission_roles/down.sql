-- Back to one role per permission; the lowest-numbered role is kept.
ALTER TABLE settings
  ADD COLUMN role_event BIGINT REFERENCES roles(id) ON DELETE SET NULL,
  ADD COLUMN role_edit BIGINT REFERENCES roles(id) ON DELETE SET NULL,
  ADD COLUMN role_delete BIGINT REFERENCES roles(id) ON DELETE SET NULL,
  ADD COLUMN role_channel BIGINT REFERENCES roles(id) ON DELETE SET NULL,
  ADD COLUMN role_signup BIGINT REFERENCES roles(id) ON DELETE SET NULL;

UPDATE settings SET
  role_event = (SELECT min(role_id) FROM permission_roles p WHERE p.guild_id = settings.guild_id AND p.kind = 'event'),
  role_edit = (SELECT min(role_id) FROM permission_roles p WHERE p.guild_id = settings.guild_id AND p.kind = 'edit'),
  role_delete = (SELECT min(role_id) FROM permission_roles p WHERE p.guild_id = settings.guild_id AND p.kind = 'delete'),
  role_channel = (SELECT min(role_id) FROM permission_roles p WHERE p.guild_id = settings.guild_id AND p.kind = 'channel'),
  role_signup = (SELECT min(role_id) FROM permission_roles p WHERE p.guild_id = settings.guild_id AND p.kind = 'signup');

DROP TABLE permission_roles;
