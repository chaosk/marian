UPDATE signup_options AS opt
SET role_id = NULL
WHERE role_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = opt.role_id);

ALTER TABLE signup_options
  ADD CONSTRAINT signup_options_role_id_fkey
  FOREIGN KEY (role_id) REFERENCES roles(id) ON DELETE SET NULL;

UPDATE settings SET role_event = NULL
WHERE role_event IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = settings.role_event);
UPDATE settings SET role_edit = NULL
WHERE role_edit IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = settings.role_edit);
UPDATE settings SET role_delete = NULL
WHERE role_delete IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = settings.role_delete);
UPDATE settings SET role_channel = NULL
WHERE role_channel IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = settings.role_channel);
UPDATE settings SET role_signup = NULL
WHERE role_signup IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM roles WHERE roles.id = settings.role_signup);

ALTER TABLE settings
  ADD CONSTRAINT settings_role_event_fkey
  FOREIGN KEY (role_event) REFERENCES roles(id) ON DELETE SET NULL,
  ADD CONSTRAINT settings_role_edit_fkey
  FOREIGN KEY (role_edit) REFERENCES roles(id) ON DELETE SET NULL,
  ADD CONSTRAINT settings_role_delete_fkey
  FOREIGN KEY (role_delete) REFERENCES roles(id) ON DELETE SET NULL,
  ADD CONSTRAINT settings_role_channel_fkey
  FOREIGN KEY (role_channel) REFERENCES roles(id) ON DELETE SET NULL,
  ADD CONSTRAINT settings_role_signup_fkey
  FOREIGN KEY (role_signup) REFERENCES roles(id) ON DELETE SET NULL;

ALTER TABLE command_log ALTER COLUMN membership_id DROP NOT NULL;
UPDATE command_log
SET membership_id = NULL
WHERE membership_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM memberships WHERE memberships.id = command_log.membership_id);

ALTER TABLE command_log
  ADD CONSTRAINT command_log_membership_id_fkey
  FOREIGN KEY (membership_id) REFERENCES memberships(id) ON DELETE SET NULL;

DELETE FROM event_prompts
WHERE NOT EXISTS (SELECT 1 FROM memberships WHERE memberships.id = event_prompts.membership_id)
   OR NOT EXISTS (SELECT 1 FROM guilds WHERE guilds.id = event_prompts.guild_id);

ALTER TABLE event_prompts
  ADD CONSTRAINT event_prompts_membership_id_fkey
  FOREIGN KEY (membership_id) REFERENCES memberships(id) ON DELETE CASCADE,
  ADD CONSTRAINT event_prompts_guild_id_fkey
  FOREIGN KEY (guild_id) REFERENCES guilds(id) ON DELETE CASCADE;

DELETE FROM web_sessions
WHERE NOT EXISTS (SELECT 1 FROM memberships WHERE memberships.id = web_sessions.membership_id)
   OR NOT EXISTS (SELECT 1 FROM guilds WHERE guilds.id = web_sessions.guild_id);

ALTER TABLE web_sessions
  ADD CONSTRAINT web_sessions_membership_id_fkey
  FOREIGN KEY (membership_id) REFERENCES memberships(id) ON DELETE CASCADE,
  ADD CONSTRAINT web_sessions_guild_id_fkey
  FOREIGN KEY (guild_id) REFERENCES guilds(id) ON DELETE CASCADE;

DELETE FROM web_session_guilds
WHERE NOT EXISTS (SELECT 1 FROM guilds WHERE guilds.id = web_session_guilds.guild_id);

ALTER TABLE web_session_guilds
  ADD CONSTRAINT web_session_guilds_guild_id_fkey
  FOREIGN KEY (guild_id) REFERENCES guilds(id) ON DELETE CASCADE;

ALTER TABLE events DROP CONSTRAINT events_created_by_fkey;
ALTER TABLE events ALTER COLUMN created_by DROP NOT NULL;
ALTER TABLE events
  ADD CONSTRAINT events_created_by_fkey
  FOREIGN KEY (created_by) REFERENCES memberships(id) ON DELETE SET NULL;

ALTER TABLE routines DROP CONSTRAINT routines_created_by_fkey;
ALTER TABLE routines ALTER COLUMN created_by DROP NOT NULL;
ALTER TABLE routines
  ADD CONSTRAINT routines_created_by_fkey
  FOREIGN KEY (created_by) REFERENCES memberships(id) ON DELETE SET NULL;

ALTER TABLE user_roles RENAME TO membership_roles;
ALTER TABLE membership_roles RENAME CONSTRAINT user_roles_pkey TO membership_roles_pkey;
ALTER TABLE membership_roles RENAME CONSTRAINT user_roles_user_id_fkey TO membership_roles_membership_id_fkey;
ALTER TABLE membership_roles RENAME CONSTRAINT user_roles_role_id_fkey TO membership_roles_role_id_fkey;
