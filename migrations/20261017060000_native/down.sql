ALTER TABLE events
  ALTER COLUMN public_id TYPE text USING public_id::text;

ALTER TABLE series
  ALTER COLUMN public_id TYPE text USING public_id::text;

ALTER TABLE routines
  ALTER COLUMN public_id TYPE text USING public_id::text;

ALTER TABLE presets
  ALTER COLUMN public_id TYPE text USING public_id::text;

ALTER TABLE routines
  ALTER COLUMN days_json TYPE jsonb USING to_jsonb(days_json);

ALTER TABLE routine_runs
  ALTER COLUMN days_json TYPE jsonb USING to_jsonb(days_json);

ALTER TABLE event_templates
  ALTER COLUMN mention_role_ids DROP DEFAULT,
  ALTER COLUMN restrict_role_ids DROP DEFAULT;

ALTER TABLE event_templates
  ALTER COLUMN mention_role_ids TYPE jsonb USING to_jsonb(mention_role_ids),
  ALTER COLUMN restrict_role_ids TYPE jsonb USING to_jsonb(restrict_role_ids),
  ALTER COLUMN mention_role_ids SET DEFAULT '[]'::jsonb,
  ALTER COLUMN restrict_role_ids SET DEFAULT '[]'::jsonb;
