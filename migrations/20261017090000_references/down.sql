ALTER TABLE membership_roles RENAME CONSTRAINT membership_roles_role_id_fkey TO user_roles_role_id_fkey;
ALTER TABLE membership_roles RENAME CONSTRAINT membership_roles_membership_id_fkey TO user_roles_user_id_fkey;
ALTER TABLE membership_roles RENAME CONSTRAINT membership_roles_pkey TO user_roles_pkey;
ALTER TABLE membership_roles RENAME TO user_roles;

ALTER TABLE routines DROP CONSTRAINT routines_created_by_fkey;
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM routines WHERE created_by IS NULL) THEN
    RAISE EXCEPTION 'A routine has no creator; the previous schema requires one';
  END IF;
END $$;
ALTER TABLE routines ALTER COLUMN created_by SET NOT NULL;
ALTER TABLE routines
  ADD CONSTRAINT routines_created_by_fkey
  FOREIGN KEY (created_by) REFERENCES memberships(id);

ALTER TABLE events DROP CONSTRAINT events_created_by_fkey;
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM events WHERE created_by IS NULL) THEN
    RAISE EXCEPTION 'An event has no creator; the previous schema requires one';
  END IF;
END $$;
ALTER TABLE events ALTER COLUMN created_by SET NOT NULL;
ALTER TABLE events
  ADD CONSTRAINT events_created_by_fkey
  FOREIGN KEY (created_by) REFERENCES memberships(id);

ALTER TABLE web_session_guilds DROP CONSTRAINT web_session_guilds_guild_id_fkey;
ALTER TABLE web_sessions DROP CONSTRAINT web_sessions_guild_id_fkey;
ALTER TABLE web_sessions DROP CONSTRAINT web_sessions_membership_id_fkey;
ALTER TABLE event_prompts DROP CONSTRAINT event_prompts_guild_id_fkey;
ALTER TABLE event_prompts DROP CONSTRAINT event_prompts_membership_id_fkey;

ALTER TABLE command_log DROP CONSTRAINT command_log_membership_id_fkey;
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM command_log WHERE membership_id IS NULL) THEN
    RAISE EXCEPTION 'A command has no membership; the previous schema requires one';
  END IF;
END $$;
ALTER TABLE command_log ALTER COLUMN membership_id SET NOT NULL;

ALTER TABLE settings DROP CONSTRAINT settings_role_signup_fkey;
ALTER TABLE settings DROP CONSTRAINT settings_role_channel_fkey;
ALTER TABLE settings DROP CONSTRAINT settings_role_delete_fkey;
ALTER TABLE settings DROP CONSTRAINT settings_role_edit_fkey;
ALTER TABLE settings DROP CONSTRAINT settings_role_event_fkey;
ALTER TABLE signup_options DROP CONSTRAINT signup_options_role_id_fkey;
