ALTER TABLE event_prompts
  ALTER COLUMN draft TYPE text USING draft::text;

ALTER TABLE presets
  ALTER COLUMN body TYPE text USING body::text;

ALTER TABLE routine_runs
  ALTER COLUMN days_json TYPE text USING days_json::text;

ALTER TABLE routines
  ALTER COLUMN days_json TYPE text USING days_json::text;

ALTER TABLE event_templates
  ALTER COLUMN options_json DROP DEFAULT,
  ALTER COLUMN reminders_json DROP DEFAULT,
  ALTER COLUMN mention_role_ids DROP DEFAULT,
  ALTER COLUMN restrict_role_ids DROP DEFAULT;

ALTER TABLE event_templates
  ALTER COLUMN options_json TYPE text USING options_json::text,
  ALTER COLUMN reminders_json TYPE text USING reminders_json::text,
  ALTER COLUMN mention_role_ids TYPE text USING mention_role_ids::text,
  ALTER COLUMN restrict_role_ids TYPE text USING restrict_role_ids::text,
  ALTER COLUMN options_json SET DEFAULT '[]',
  ALTER COLUMN reminders_json SET DEFAULT '[]',
  ALTER COLUMN mention_role_ids SET DEFAULT '[]',
  ALTER COLUMN restrict_role_ids SET DEFAULT '[]';
