-- Copies each type's name, emoji, and attending back onto what used it.

ALTER TABLE event_templates ADD COLUMN options_json jsonb NOT NULL DEFAULT '[]'::jsonb;
UPDATE event_templates t
   SET options_json = coalesce((
     SELECT jsonb_agg(jsonb_build_object(
              'id', null, 'name', l.name, 'emoji', l.emoji, 'capacity', o.capacity,
              'role_id', o.role_id, 'reminders_enabled', l.attending) ORDER BY o.position)
       FROM template_options o JOIN option_library l ON l.id = o.type_id
      WHERE o.template_id = t.id), '[]'::jsonb);
DROP TABLE template_options;

ALTER TABLE settings DROP COLUMN default_preset_id;

ALTER TABLE presets ADD COLUMN body jsonb NOT NULL DEFAULT '{}'::jsonb;
UPDATE presets p
   SET body = jsonb_build_object(
     'multiple_signups', p.multiple_signups,
     'options', coalesce((
       SELECT jsonb_agg(jsonb_build_object(
                'id', null, 'name', l.name, 'emoji', l.emoji, 'capacity', o.capacity,
                'role_id', o.role_id, 'reminders_enabled', l.attending) ORDER BY o.position)
         FROM preset_options o JOIN option_library l ON l.id = o.type_id
        WHERE o.preset_id = p.id), '[]'::jsonb));
ALTER TABLE presets DROP COLUMN multiple_signups, ALTER COLUMN public_id DROP DEFAULT;
DROP TABLE preset_options;

ALTER TABLE signup_options
  ADD COLUMN name text NOT NULL DEFAULT '',
  ADD COLUMN emoji text NOT NULL DEFAULT '',
  ADD COLUMN reminders_enabled boolean NOT NULL DEFAULT false;
UPDATE signup_options o
   SET name = l.name, emoji = l.emoji, reminders_enabled = l.attending
  FROM option_library l
 WHERE l.id = o.type_id;
DROP INDEX idx_signup_options_type;
ALTER TABLE signup_options DROP COLUMN type_id;

DROP INDEX idx_option_library_public_id;
DROP INDEX idx_option_library_guild_name;
ALTER TABLE option_library ADD CONSTRAINT option_library_guild_id_name_key UNIQUE (guild_id, name);
ALTER TABLE option_library DROP COLUMN attending, DROP COLUMN public_id;
